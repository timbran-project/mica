// Browser editor client: input normalization, provisional display, and
// viewport painting.
//
// The host slice of the editor protocol. It sends one normalized item per
// request, in order, and repaints from the authoritative snapshot Mica
// returns. Typing is painted locally first and reconciled when the result
// arrives, so a slow link does not make the keyboard feel dead; provisional
// text is never authoritative and the authoritative snapshot always wins.
//
// What lives here and nowhere else: platform key events, the composition path,
// clipboard access after a user gesture, and painting text into DOM nodes.
// Keymaps, commands, modes, and buffer policy all live in Mica.
//
// The platform input target is a hidden textarea: the visible text lives in
// the viewport elements, and the textarea exists only so `beforeinput`,
// composition, and paste fire where input methods expect them.

const SNAPSHOT_LINES = 200;
const SNAPSHOT_SCALARS = 262144;
const REQUEST_TIMEOUT_MS = 15000;

const NAMED_KEYS = {
    ArrowLeft: "<left>",
    ArrowRight: "<right>",
    ArrowUp: "<up>",
    ArrowDown: "<down>",
    Enter: "<return>",
    Backspace: "<backspace>",
    Delete: "<delete>",
    Home: "<home>",
    End: "<end>",
    Escape: "<escape>",
    " ": "<space>",
    Tab: "<tab>",
};

// Keys that must not reach the browser even when unmodified.
const SWALLOWED = new Set([
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "ArrowDown",
    "Enter",
    "Backspace",
    "Tab",
]);

// The chord one keydown event denotes. Plain printable keys return null unless
// `force` asks for them: as text they must come from `beforeinput`, but as the
// next chord of a pending prefix they are keys.
export function chordFor(event, isMac, force = false) {
    const mods = [];
    if (event.ctrlKey) mods.push("C");
    if (event.altKey) mods.push("M");
    if (isMac && event.metaKey) mods.push("s");
    const named = NAMED_KEYS[event.key];
    let base = named;
    if (!base && event.key.length === 1) {
        base = event.key;
        if (event.ctrlKey || event.altKey) base = base.toLowerCase();
    }
    if (!base) return null;
    if (event.shiftKey && !named && base.length === 1 && base.toLowerCase() !== base.toUpperCase()) {
        mods.push("S");
        base = base.toLowerCase();
    }
    if (mods.length === 0 && !named && !force) return null;
    return mods.map((mod) => mod + "-").join("") + base;
}

// Scalars in `text` before UTF-16 offset `utf16`.
export function scalarPrefixLength(text, utf16) {
    let scalars = 0;
    for (let index = 0; index < utf16 && index < text.length;) {
        const code = text.codePointAt(index);
        index += code > 0xffff ? 2 : 1;
        scalars += 1;
    }
    return scalars;
}

// Converts a Unicode scalar column to the UTF-16 offset JavaScript strings
// and DOM ranges use.
export function utf16OffsetForScalar(text, scalar) {
    let utf16 = 0;
    let count = 0;
    while (utf16 < text.length && count < scalar) {
        const code = text.codePointAt(utf16);
        utf16 += code > 0xffff ? 2 : 1;
        count += 1;
    }
    return utf16;
}

function scalarLength(text) {
    return Array.from(text).length;
}

function randomSessionId(cryptoImpl = globalThis.crypto) {
    if (cryptoImpl && typeof cryptoImpl.getRandomValues === "function") {
        const words = new Uint32Array(2);
        cryptoImpl.getRandomValues(words);
        // Keep the value within Mica's exact positive integer range.
        const value = (BigInt(words[0] & 0x001fffff) << 32n) | BigInt(words[1]);
        return String(value || 1n);
    }
    return String(Math.floor(Math.random() * Number.MAX_SAFE_INTEGER) + 1);
}

// UTF-16 offset within a line element, by walking its child nodes. A click in
// the blank area to the right of the text belongs to the line element itself
// and resolves to the line's end.
export function utf16OffsetInLine(line, node, offset) {
    if (node === line) return line.textContent.length;
    let total = 0;
    for (const child of line.childNodes) {
        if (child === node) return total + offset;
        total += child.textContent.length;
    }
    return null;
}

// Applies not-yet-acknowledged insertions to a copy of `rows`, returning the
// text and caret the server state will settle at. Only used for display.
export function replayInsertions(rows, line, column, texts) {
    const copy = rows.map((row) => ({ ...row }));
    for (const text of texts) {
        const index = copy.findIndex((row) => row.line === line);
        if (index < 0) return { rows: copy, line, column };
        if (text.includes("\n")) {
            const parts = text.split("\n");
            const utf16 = utf16OffsetForScalar(copy[index].text, column);
            const before = copy[index].text.slice(0, utf16);
            const after = copy[index].text.slice(utf16);
            copy[index].text = before + parts[0];
            let insertAt = index + 1;
            for (let part = 1; part < parts.length; part += 1) {
                const last = part === parts.length - 1;
                copy.splice(insertAt, 0, {
                    line: line + part,
                    start: 0,
                    stop: 0,
                    text: last ? parts[part] + after : parts[part],
                    complete: true,
                });
                insertAt += 1;
            }
            const added = parts.length - 1;
            for (let rest = insertAt; rest < copy.length; rest += 1) copy[rest].line += added;
            line += parts.length - 1;
            column = scalarLength(parts[parts.length - 1]);
            continue;
        }
        const current = copy[index];
        const utf16 = utf16OffsetForScalar(current.text, column);
        current.text = current.text.slice(0, utf16) + text + current.text.slice(utf16);
        column += scalarLength(text);
    }
    return { rows: copy, line, column };
}

function scalarSlice(text, start, stop = undefined) {
    return Array.from(text).slice(start, stop).join("");
}

function rowSegment(rows) {
    if (!Array.isArray(rows) || rows.length === 0) return null;
    let text = "";
    for (let index = 0; index < rows.length; index += 1) {
        const row = rows[index];
        if (index > 0) {
            const previous = rows[index - 1];
            if (row.line !== previous.line + 1 || row.start !== previous.stop + 1) return null;
            text += "\n";
        }
        text += String(row.text || "");
    }
    return {
        start: Number(rows[0].start || 0),
        firstLine: Number(rows[0].line || 0),
        text,
        lastComplete: rows[rows.length - 1].complete !== false,
    };
}

function rowsFromSegment(segment, text) {
    const parts = String(text).split("\n");
    let start = segment.start;
    return parts.map((part, index) => {
        const length = scalarLength(part);
        const row = {
            line: segment.firstLine + index,
            start,
            stop: start + length,
            text: part,
            complete: index === parts.length - 1 ? segment.lastComplete : true,
        };
        start += length + 1;
        return row;
    });
}

// Applies authoritative scalar edits to a bounded viewport replica. The
// caller requests a new snapshot when an edit reaches outside the replica.
export function applyEditsToRows(rows, edits) {
    const segment = rowSegment(rows);
    if (!segment) return { rows, ok: false };
    let scalars = Array.from(segment.text);
    const ordered = [...(edits || [])].sort((a, b) => Number(b.at) - Number(a.at));
    for (const edit of ordered) {
        const at = Number(edit.at);
        const remove = Number(edit.remove || 0);
        const local = at - segment.start;
        if (local < 0 || remove < 0 || local + remove > scalars.length) {
            return { rows, ok: false };
        }
        scalars.splice(local, remove, ...Array.from(String(edit.text || "")));
    }
    return { rows: rowsFromSegment(segment, scalars.join("")), ok: true };
}

function offsetForPosition(rows, line, column) {
    const row = rows.find((entry) => Number(entry.line) === Number(line));
    if (!row) return null;
    return Number(row.start) + Math.max(0, Math.min(Number(column), scalarLength(row.text || "")));
}

function positionForOffset(rows, offset) {
    const target = Number(offset);
    for (const row of rows) {
        if (target >= Number(row.start) && target <= Number(row.stop)) {
            return { line: Number(row.line), column: target - Number(row.start) };
        }
    }
    return null;
}

function predictionText(entry) {
    const item = entry.item || {};
    if (item.kind === "text" || item.kind === "paste") return String(item.text || "");
    if (
        item.kind === "input"
        && (item.input_type === "insertText" || item.input_type === "insertCompositionText")
    ) {
        return String(item.text || "");
    }
    if (
        (item.kind === "input" && item.input_type === "insertLineBreak")
        || (item.kind === "key" && ["<return>", "C-j", "C-m"].includes(item.key))
    ) {
        return "\n";
    }
    return "";
}

// Replays the fixed browser-side predictor vocabulary over an authoritative
// viewport. Mica selects predictor names through its keymap plan; JavaScript
// only implements these mechanical text and position operations.
export function replayPredictions(rows, pointLine, pointColumn, entries, initialMark = null) {
    let copy = rows.map((row) => ({ ...row }));
    let point = offsetForPosition(copy, pointLine, pointColumn);
    let mark = initialMark;
    let markActive = mark !== null;
    let goalColumn = null;
    let complete = point !== null;
    const edits = [];

    const applyEdit = (at, remove, text, nextPoint) => {
        const applied = applyEditsToRows(copy, [{ at, remove, text }]);
        if (!applied.ok) {
            complete = false;
            return;
        }
        copy = applied.rows;
        edits.push({ at, remove, text });
        const inserted = scalarLength(text);
        if (mark !== null) {
            if (mark > at + remove) mark += inserted - remove;
            else if (mark >= at) mark = at + inserted;
        }
        point = nextPoint;
    };

    for (const entry of entries || []) {
        if (!complete) break;
        const predictor = entry.predictor || "none";
        switch (predictor) {
            case "insert_text": {
                const text = predictionText(entry);
                applyEdit(point, 0, text, point + scalarLength(text));
                goalColumn = null;
                break;
            }
            case "delete_backward_scalar":
                if (point > Number(copy[0].start)) applyEdit(point - 1, 1, "", point - 1);
                goalColumn = null;
                break;
            case "delete_forward_scalar": {
                const last = copy[copy.length - 1];
                if (point < Number(last.stop)) applyEdit(point, 1, "", point);
                goalColumn = null;
                break;
            }
            case "move_forward_scalar": {
                const last = copy[copy.length - 1];
                point = Math.min(point + 1, Number(last.stop));
                goalColumn = null;
                break;
            }
            case "move_backward_scalar":
                point = Math.max(point - 1, Number(copy[0].start));
                goalColumn = null;
                break;
            case "move_line_start": {
                const position = positionForOffset(copy, point);
                point = offsetForPosition(copy, position.line, 0);
                goalColumn = null;
                break;
            }
            case "move_line_end": {
                const position = positionForOffset(copy, point);
                const row = copy.find((candidate) => candidate.line === position.line);
                point = Number(row.stop);
                goalColumn = null;
                break;
            }
            case "move_logical_line_down":
            case "move_logical_line_up": {
                const position = positionForOffset(copy, point);
                if (goalColumn === null) goalColumn = position.column;
                const direction = predictor === "move_logical_line_down" ? 1 : -1;
                const moved = offsetForPosition(copy, position.line + direction, goalColumn);
                if (moved !== null) point = moved;
                break;
            }
            case "set_mark":
                mark = point;
                markActive = true;
                break;
            case "exchange_point_mark":
                if (mark !== null) {
                    const oldPoint = point;
                    point = mark;
                    mark = oldPoint;
                    markActive = true;
                }
                break;
            case "set_point": {
                const target = Number(entry.item && entry.item.scalar_offset);
                if (positionForOffset(copy, target)) point = target;
                break;
            }
            case "none":
            case "scroll_lines":
                break;
            default:
                complete = false;
                break;
        }
        if (entry.barrier) break;
    }

    const placed = positionForOffset(copy, point);
    if (!placed) complete = false;
    return {
        rows: copy,
        line: placed ? placed.line : pointLine,
        column: placed ? placed.column : pointColumn,
        mark,
        markActive,
        complete,
        edits,
    };
}

// Installs the editor into `root`. Everything the client touches is injected,
// so a DOM stub can drive the real event paths in tests.
export function createEditor(options = {}) {
    const doc = options.document ?? globalThis.document;
    const win = options.window ?? globalThis.window;
    const nav = options.navigator ?? globalThis.navigator ?? {};
    const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
    const EventSourceImpl = options.EventSource ?? globalThis.EventSource;
    const measureViewports = options.measureViewports !== false;
    const root = options.root ?? doc.getElementById("mica-editor");
    if (!root) return null;
    const debug = options.debug === true && typeof console !== "undefined" && !!console.debug;

    const session = String(options.session || root.dataset.session || randomSessionId(options.crypto));
    const platform = (nav.userAgentData && nav.userAgentData.platform) || nav.platform || "";
    const isMac = /Mac|iPhone|iPad/.test(platform);

    const state = {
        session,
        nextSequence: 1,
        authoritativePending: "",
        pending: "",
        inFlight: false,
        inFlightItem: null,
        queue: [],
        outstanding: [],
        resultBuffer: new Map(),
        throughSequence: 0,
        asyncMode: typeof EventSourceImpl === "function",
        composing: false,
        composingText: "",
        message: "",
        error: false,
        blocked: false,
        baseRows: [],
        basePoint: { line: 0, column: 0 },
        baseMark: null,
        selectedWindow: null,
        windowStates: new Map(),
        windowElements: new Map(),
        reportedViewportSizes: new Map(),
        keymapPlan: [],
        snapshot: null,
        pointLine: 0,
        pointColumn: 0,
        provisionalText: "",
    };

    let viewport;
    let modeline;
    let echo;
    let inputTarget;
    let frameRoot;
    let eventSource = null;
    let flushScheduled = false;
    let draggingSplit = null;
    let viewportMeasurementScheduled = false;
    const postingBatches = new Map();

    function buildChrome() {
        frameRoot = doc.createElement("div");
        frameRoot.className = "editor-frame";
        const panel = doc.createElement("div");
        panel.className = "editor-window selected";
        viewport = doc.createElement("pre");
        viewport.className = "editor-viewport";
        viewport.setAttribute("role", "textbox");
        viewport.setAttribute("aria-multiline", "true");

        modeline = doc.createElement("div");
        modeline.className = "editor-modeline";
        panel.append(viewport, modeline);
        frameRoot.appendChild(panel);
        state.windowElements.set("1", { panel, viewport, modeline });

        echo = doc.createElement("div");
        echo.className = "editor-echo";

        inputTarget = doc.createElement("textarea");
        inputTarget.className = "editor-input";
        inputTarget.autocapitalize = "off";
        inputTarget.autocomplete = "off";
        inputTarget.spellcheck = false;
        inputTarget.setAttribute("autocorrect", "off");
        inputTarget.setAttribute("aria-label", "editor input");

        root.replaceChildren(frameRoot, echo, inputTarget);
        inputTarget.focus();
    }

    function snapshotUrl() {
        return (
            `/editor/snapshot?session=${encodeURIComponent(state.session)}`
            + `&lines=${SNAPSHOT_LINES}&max=${SNAPSHOT_SCALARS}`
        );
    }

    function send(item) {
        if (state.blocked) return;
        state.queue.push({ sequence: state.nextSequence, item, ...predictionFor(item) });
        state.nextSequence += 1;
        updatePredictedPending();
        if (debug) console.debug("mica editor item", item);
        paintProvisional();
        applyScrollPrediction(state.queue[state.queue.length - 1]);
        if (state.asyncMode) scheduleFlush();
        else pump();
    }

    function scheduleFlush() {
        if (state.blocked) return;
        if (flushScheduled) return;
        flushScheduled = true;
        Promise.resolve().then(() => {
            flushScheduled = false;
            if (state.blocked) return;
            while (state.queue.length > 0) {
                const entries = state.queue.splice(0, 256);
                state.outstanding.push(...entries);
                postBatch(entries);
            }
            updatePredictedPending();
        });
    }

    function batchBody(entries) {
        const generation = String((state.snapshot && state.snapshot.keymap_generation) || 0);
        return {
            type: "editor_input",
            session: state.session,
            items: entries.map((entry) => ({
                ...entry.item,
                sequence: String(entry.sequence),
                depends_on: String(entry.sequence - 1),
                frame: "1",
                keymap_generation: generation,
            })),
        };
    }

    async function postBatch(entries) {
        if (state.blocked) return;
        entries = entries.filter((entry) => entry.sequence > state.throughSequence);
        if (entries.length === 0) return;
        const key = entries[0].sequence;
        if (postingBatches.has(key)) return;
        postingBatches.set(key, entries);
        try {
            const response = await request("/editor/input", {
                method: "POST",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(batchBody(entries)),
            });
            if (stopForPermanentFailure(response)) {
                postingBatches.delete(key);
                return;
            }
            if (!response.ok) throw new Error(`HTTP ${response.status}`);
            postingBatches.delete(key);
        } catch (error) {
            postingBatches.delete(key);
            if (state.blocked) return;
            state.message = `connection interrupted: ${error}`;
            state.error = true;
            renderEcho();
            setTimeout(() => postBatch(entries), 50);
        }
    }

    function replayOutstanding() {
        const unsent = state.outstanding.filter(
            (entry) => !postingBatches.has(entry.sequence),
        );
        for (let index = 0; index < unsent.length; index += 256) {
            postBatch(unsent.slice(index, index + 256));
        }
    }

    function receiveAsyncResult(data) {
        if (state.blocked) return;
        const sequence = Number(data && data.through_sequence);
        if (!Number.isSafeInteger(sequence) || sequence <= state.throughSequence) return;
        state.resultBuffer.set(sequence, data);
        while (state.resultBuffer.has(state.throughSequence + 1)) {
            const next = state.throughSequence + 1;
            const current = state.resultBuffer.get(next);
            state.resultBuffer.delete(next);
            const pendingIndex = state.outstanding.findIndex((entry) => entry.sequence === next);
            if (pendingIndex >= 0) state.outstanding.splice(pendingIndex, 1);
            state.throughSequence = next;
            applyResult(current.result);
            if (current.snapshot) {
                render(current.snapshot);
            } else if (applyAuthoritativeResult(current.result)) {
                paintProvisional();
            } else {
                resync();
            }
        }
    }

    function connectEvents() {
        if (!state.asyncMode || eventSource) return;
        eventSource = new EventSourceImpl(
            `/sync/events?session=${encodeURIComponent(state.session)}`,
        );
        let opened = false;
        eventSource.addEventListener("open", () => {
            if (state.blocked) return;
            if (opened) replayOutstanding();
            opened = true;
            state.error = false;
            renderEcho();
        });
        eventSource.addEventListener("editor", (event) => {
            try {
                receiveAsyncResult(JSON.parse(event.data));
            } catch (error) {
                state.message = `invalid editor result: ${error}`;
                state.error = true;
                renderEcho();
            }
        });
        eventSource.addEventListener("error", () => {
            if (state.blocked) return;
            state.message = "editor result stream interrupted; reconnecting";
            state.error = true;
            renderEcho();
        });
    }

    async function pump() {
        if (state.blocked) return;
        if (state.inFlight || (state.inFlightItem === null && state.queue.length === 0)) return;
        state.inFlight = true;
        const queued = state.inFlightItem || state.queue.shift();
        state.inFlightItem = queued;
        const url = `/editor/input?session=${encodeURIComponent(state.session)}&frame=1`
            + `&sequence=${queued.sequence}&lines=${SNAPSHOT_LINES}&max=${SNAPSHOT_SCALARS}`;
        try {
            const response = await request(url, {
                method: "POST",
                headers: { "content-type": "application/json" },
                body: JSON.stringify(queued.item),
            });
            if (stopForPermanentFailure(response)) {
                state.inFlight = false;
                return;
            }
            if (!response.ok) throw new Error(`HTTP ${response.status}`);
            const data = await response.json();
            if (debug) console.debug("mica editor result", data.result && data.result.status, data.snapshot);
            state.inFlightItem = null;
            applyResult(data.result);
            if (data.snapshot) {
                render(data.snapshot);
            } else if (applyAuthoritativeResult(data.result)) {
                paintProvisional();
            } else {
                await resync();
            }
        } catch (error) {
            if (state.blocked) return;
            state.message = `connection lost: ${error}`;
            state.error = true;
            renderEcho();
            state.inFlight = false;
            // Keep the item and its sequence. A retry is safe because the host
            // replays a completed result or resumes an incomplete finalization.
            await resync();
            pump();
            return;
        }
        state.inFlight = false;
        pump();
    }

    function stopForPermanentFailure(response) {
        if (![400, 401, 403, 410, 413, 415].includes(response.status)) return false;
        state.blocked = true;
        state.error = true;
        state.message = `Input stopped (HTTP ${response.status}). Review unconfirmed edits before reloading.`;
        if (eventSource) eventSource.close();
        renderEcho();
        return true;
    }

    // A fetch with a deadline: a dropped connection must not wedge the queue.
    async function request(url, init) {
        if (typeof AbortController === "undefined") return fetchImpl(url, init);
        const controller = new AbortController();
        const timer = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
        try {
            return await fetchImpl(url, { ...init, signal: controller.signal });
        } finally {
            clearTimeout(timer);
        }
    }

    async function resync() {
        if (state.blocked) return;
        try {
            const response = await request(snapshotUrl(), {});
            if (response.ok) render(await response.json());
        } catch {
            // Stay quiet: the echo area already reports the lost connection.
        }
    }

    function applyResult(result) {
        if (!result) return;
        state.error = result.status === "rejected" || result.status === "resync";
        if (result.message && result.message !== "none") {
            state.message = result.message;
        } else if (result.status === "undefined") {
            state.message = "undefined key sequence";
        } else if (result.status === "ok") {
            state.message = "";
        }
    }

    function predictedPendingAfter(pending, item) {
        if (!item || item.kind !== "key") return "";
        const sequence = pending ? `${pending} ${item.key}` : item.key;
        const plan = state.keymapPlan;
        const prefix = `${sequence} `;
        if (plan.some((row) => String(row.sequence || "").startsWith(prefix))) return sequence;
        return "";
    }

    function predictionFor(item) {
        if (!item) return { predictor: "none", barrier: true };
        if (item.kind === "text" || item.kind === "paste") {
            return { predictor: "insert_text", barrier: false };
        }
        if (item.kind === "input") {
            if (
                item.input_type === "insertText" || item.input_type === "insertCompositionText"
                || item.input_type === "insertLineBreak"
            ) {
                return { predictor: "insert_text", barrier: false };
            }
            if (item.input_type === "deleteContentBackward") {
                return { predictor: "delete_backward_scalar", barrier: false };
            }
            if (item.input_type === "deleteContentForward") {
                return { predictor: "delete_forward_scalar", barrier: false };
            }
        }
        if (item.kind === "viewport") return { predictor: "none", barrier: false };
        if (item.kind === "pointer") {
            if (item.window !== undefined && String(item.window) !== String(state.selectedWindow)) {
                return { predictor: "none", barrier: true };
            }
            return { predictor: "set_point", barrier: false };
        }
        if (item.kind === "key") {
            const sequence = state.pending ? `${state.pending} ${item.key}` : item.key;
            const plan = state.keymapPlan.find((row) => String(row.sequence || "") === sequence);
            if (plan) {
                return {
                    predictor: String(plan.predictor || "none"),
                    barrier: plan.barrier === true,
                };
            }
            const prefix = `${sequence} `;
            if (state.keymapPlan.some((row) => String(row.sequence || "").startsWith(prefix))) {
                return { predictor: "none", barrier: false };
            }
        }
        return { predictor: "none", barrier: true };
    }

    function applyScrollPrediction(entry) {
        if (!entry || entry.predictor !== "scroll_lines" || !viewport) return;
        const key = entry.item && entry.item.key;
        if (key === "C-v") {
            viewport.scrollTop += viewport.clientHeight;
        } else if (key === "M-v") {
            viewport.scrollTop = Math.max(0, viewport.scrollTop - viewport.clientHeight);
        } else if (key === "C-l") {
            const caret = viewport.querySelector(".editor-caret");
            if (caret && caret.parentElement) {
                viewport.scrollTop = Math.max(
                    0,
                    caret.parentElement.offsetTop - Math.floor(viewport.clientHeight / 2),
                );
            }
        }
    }

    function updatePredictedPending() {
        let pending = state.authoritativePending;
        if (state.inFlightItem) pending = predictedPendingAfter(pending, state.inFlightItem.item);
        for (const queued of state.outstanding) pending = predictedPendingAfter(pending, queued.item);
        for (const queued of state.queue) pending = predictedPendingAfter(pending, queued.item);
        state.pending = pending;
    }

    // The items that are sent but not yet acknowledged, if they are all plain
    // text insertions. Anything else disables replay until the queue drains.
    function pendingText() {
        const items = [];
        if (state.inFlightItem !== null) {
            if (state.inFlightItem.item.kind !== "text") return null;
            items.push(state.inFlightItem.item);
        }
        for (const queued of state.outstanding) {
            if (queued.item.kind !== "text") return null;
            items.push(queued.item);
        }
        for (const queued of state.queue) {
            if (queued.item.kind !== "text") return null;
            items.push(queued.item);
        }
        return items.map((item) => String(item.text || ""));
    }

    function pendingEntries() {
        const entries = [];
        if (state.inFlightItem !== null) entries.push(state.inFlightItem);
        entries.push(...state.outstanding);
        entries.push(...state.queue);
        return entries;
    }

    function applyAuthoritativeResult(result) {
        if (!result || result.status === "resync") return false;
        const edits = Array.isArray(result.edits) ? result.edits : [];
        const affectedBuffer = String(result.buffer ?? "");
        if (edits.length > 0) {
            for (const [key, windowState] of state.windowStates) {
                if (String(windowState.buffer ?? "") !== affectedBuffer) continue;
                const applied = applyEditsToRows(windowState.rows || [], edits);
                if (!applied.ok) return false;
                state.windowStates.set(key, { ...windowState, rows: applied.rows });
            }
        }

        if (Array.isArray(result.windows)) {
            for (const summary of result.windows) {
                const key = String(summary.window);
                const previous = state.windowStates.get(key);
                if (!previous) return false;
                const rows = previous.rows || [];
                if (
                    summary.first_line !== undefined && rows.length > 0
                    && Number(summary.first_line) !== Number(rows[0].line)
                ) {
                    return false;
                }
                state.windowStates.set(key, { ...previous, ...summary, rows });
            }
        }

        const selectedKey = String(result.selected_window ?? result.window ?? state.selectedWindow);
        const selected = state.windowStates.get(selectedKey);
        if (!selected) return false;
        const selectedRows = selected.rows || [];
        const line = Number(result.point_line ?? selected.point_line);
        const column = Number(result.point_column ?? selected.point_column);
        if (
            !Number.isFinite(line) || !Number.isFinite(column)
            || offsetForPosition(selectedRows, line, column) === null
        ) {
            return false;
        }
        state.windowStates.set(selectedKey, {
            ...selected,
            ...result,
            rows: selectedRows,
            point_line: line,
            point_column: column,
        });
        state.selectedWindow = selectedKey;
        state.authoritativePending = result.pending || "";
        if (Array.isArray(result.keymap_plan)) state.keymapPlan = result.keymap_plan;
        state.snapshot = {
            ...(state.snapshot || {}),
            ...result,
            selected_window: result.selected_window ?? result.window,
            windows: Array.from(state.windowStates.values()),
            rows: selectedRows,
            point_line: line,
            point_column: column,
            keymap_plan: state.keymapPlan,
        };
        syncSelectedWindow();
        updatePredictedPending();
        return true;
    }

    function rebaseOffset(offset, edits) {
        let rebased = Number(offset);
        for (const edit of edits) {
            const at = Number(edit.at);
            const remove = Number(edit.remove || 0);
            const inserted = scalarLength(String(edit.text || ""));
            if (rebased > at + remove) rebased += inserted - remove;
            else if (rebased >= at) rebased = at + inserted;
        }
        return rebased;
    }

    function syncSelectedWindow() {
        const selected = state.windowStates.get(String(state.selectedWindow));
        if (!selected) return;
        state.baseRows = selected.rows || [];
        state.basePoint = {
            line: Number(selected.point_line || 0),
            column: Number(selected.point_column || 0),
        };
        state.baseMark = selected.mark_active ? Number(selected.mark) : null;
        for (const [key, elements] of state.windowElements) {
            const active = key === String(state.selectedWindow);
            elements.panel.className = active ? "editor-window selected" : "editor-window";
            if (active) {
                viewport = elements.viewport;
                modeline = elements.modeline;
            }
        }
    }

    function selectWindowLocally(window) {
        const key = String(window);
        if (key === String(state.selectedWindow) || !state.windowStates.has(key)) return;
        // Earlier items still belong to the old selected window. Let Mica apply
        // them before the browser changes its provisional window.
        if (pendingEntries().length > 0) return;
        state.selectedWindow = key;
        state.snapshot = { ...(state.snapshot || {}), selected_window: window };
        syncSelectedWindow();
    }

    function paintProvisional() {
        const inMinibuffer = !!(state.snapshot && state.snapshot.minibuffer_active);
        // Text typed at a prompt belongs to the prompt, not the buffer behind it.
        const texts = pendingText() ?? [];
        state.provisionalText = inMinibuffer ? texts.join("") : "";
        let rows = state.baseRows;
        let line = state.basePoint.line;
        let column = state.basePoint.column;
        let predictedEdits = [];
        if (!inMinibuffer) {
            const replayed = replayPredictions(rows, line, column, pendingEntries(), state.baseMark);
            rows = replayed.rows;
            line = replayed.line;
            column = replayed.column;
            predictedEdits = replayed.edits;
        }
        state.pointLine = line;
        state.pointColumn = column;
        const selectedKey = String(state.selectedWindow);
        const selected = state.windowStates.get(selectedKey) || state.snapshot || {};
        for (const [key, elements] of state.windowElements) {
            const windowState = state.windowStates.get(key);
            if (!windowState) continue;
            if (key === selectedKey) {
                paintRows(elements.viewport, elements.modeline, selected, rows, line, column, true);
                continue;
            }
            let otherRows = windowState.rows || [];
            let otherLine = Number(windowState.point_line || 0);
            let otherColumn = Number(windowState.point_column || 0);
            if (
                !inMinibuffer && predictedEdits.length > 0
                && String(windowState.buffer) === String(selected.buffer)
            ) {
                let complete = true;
                for (const edit of predictedEdits) {
                    const applied = applyEditsToRows(otherRows, [edit]);
                    if (!applied.ok) {
                        complete = false;
                        break;
                    }
                    otherRows = applied.rows;
                }
                if (complete) {
                    const placed = positionForOffset(
                        otherRows,
                        rebaseOffset(windowState.point, predictedEdits),
                    );
                    if (placed) {
                        otherLine = placed.line;
                        otherColumn = placed.column;
                    }
                }
            }
            paintRows(
                elements.viewport,
                elements.modeline,
                windowState,
                otherRows,
                otherLine,
                otherColumn,
                false,
            );
        }
        renderEcho();
        placeInputTarget();
    }

    function render(snapshot) {
        state.session = snapshot.session || state.session;
        state.authoritativePending = snapshot.pending || "";
        state.windowStates = new Map();
        const windows = Array.isArray(snapshot.windows) && snapshot.windows.length > 0
            ? snapshot.windows
            : [snapshot];
        for (const entry of windows) {
            state.windowStates.set(String(entry.window ?? snapshot.window ?? 1), {
                ...entry,
                rows: entry.rows || [],
            });
        }
        state.selectedWindow = String(snapshot.selected_window ?? snapshot.window ?? 1);
        if (Array.isArray(snapshot.keymap_plan)) state.keymapPlan = snapshot.keymap_plan;
        state.snapshot = snapshot;
        syncSelectedWindow();
        updatePredictedPending();
        renderFrame(snapshot);
        syncSelectedWindow();
        paintProvisional();
    }

    function paintRows(
        targetViewport,
        targetModeline,
        windowSnapshot,
        rows,
        pointLine,
        pointColumn,
        showCaret = true,
    ) {
        const previous = targetViewport._editorRows || new Map();
        const next = new Map();
        const ordered = [];
        for (const row of rows) {
            const key = String(row.line);
            const line = previous.get(key) || doc.createElement("div");
            line.className = "editor-line";
            line._row = row;
            line._window = windowSnapshot.window;
            const text = row.text || "";
            const caretColumn = showCaret && row.line === pointLine ? pointColumn : -1;
            const renderKey = `${text}\u0000${caretColumn}`;
            if (line._editorRenderKey !== renderKey) {
                line.replaceChildren();
                if (caretColumn >= 0) {
                    const utf16 = utf16OffsetForScalar(text, pointColumn);
                    line.appendChild(doc.createTextNode(text.slice(0, utf16)));
                    const caret = doc.createElement("span");
                    caret.className = "editor-caret";
                    line.appendChild(caret);
                    line.appendChild(doc.createTextNode(text.slice(utf16)));
                } else {
                    line.textContent = text;
                }
                line._editorRenderKey = renderKey;
            }
            next.set(key, line);
            ordered.push(line);
        }
        if (rows.length === 0) {
            const line = previous.get("__empty") || doc.createElement("div");
            line.className = "editor-line";
            line._window = windowSnapshot.window;
            const emptyKey = showCaret ? "empty-caret" : "empty";
            if (line._editorRenderKey !== emptyKey) {
                if (showCaret) {
                    const caret = doc.createElement("span");
                    caret.className = "editor-caret";
                    line.replaceChildren(caret);
                } else {
                    line.replaceChildren();
                }
                line._editorRenderKey = emptyKey;
            }
            next.set("__empty", line);
            ordered.push(line);
        }
        const children = Array.from(targetViewport.children || []);
        const orderChanged = children.length !== ordered.length
            || ordered.some((line, index) => children[index] !== line);
        if (orderChanged) {
            targetViewport.replaceChildren(...ordered);
        }
        targetViewport._editorRows = next;
        const name = windowSnapshot.buffer_name || "*scratch*";
        const modified = windowSnapshot.modified ? " **" : "";
        const mark = windowSnapshot.mark_active ? "  mark" : "";
        const modelineText = `-UUU:----F1  ${name}${modified}  L${pointLine + 1} C${pointColumn}  (Fundamental)${mark}`;
        if (targetModeline.textContent !== modelineText) targetModeline.textContent = modelineText;
    }

    function renderFrame(snapshot) {
        if (!snapshot.frame_tree || !Array.isArray(snapshot.windows)) return;
        state.windowElements = new Map();
        let selectedViewport = null;
        let selectedModeline = null;

        function build(node) {
            if (node && node.kind === "split") {
                const split = doc.createElement("div");
                split.className = `editor-split ${node.axis || "horizontal"}`;
                const first = build(node.first);
                const second = build(node.second);
                const ratio = Math.max(1, Math.min(999, Number(node.ratio) || 500));
                first.style.flexGrow = String(ratio);
                second.style.flexGrow = String(1000 - ratio);
                const divider = doc.createElement("div");
                divider.className = `editor-divider ${node.axis || "horizontal"}`;
                divider.dataset.split = String(node.node);
                divider.setAttribute("role", "separator");
                divider.setAttribute(
                    "aria-orientation",
                    node.axis === "vertical" ? "vertical" : "horizontal",
                );
                divider.addEventListener("mousedown", (event) => {
                    event.preventDefault();
                    if (event.stopPropagation) event.stopPropagation();
                    const rect = split.getBoundingClientRect();
                    draggingSplit = {
                        split: Number(node.node),
                        axis: node.axis || "horizontal",
                        rect,
                        first,
                        second,
                        ratio,
                    };
                    inputTarget.focus();
                });
                split.append(first, divider, second);
                return split;
            }
            const key = String(node && node.window);
            const data = state.windowStates.get(key) || snapshot;
            const panel = doc.createElement("div");
            const selected = key === String(state.selectedWindow);
            panel.className = selected ? "editor-window selected" : "editor-window";
            panel.dataset.window = String(data.window);
            const view = doc.createElement("pre");
            view.className = "editor-viewport";
            view.setAttribute("role", "textbox");
            view.setAttribute("aria-multiline", "true");
            const mode = doc.createElement("div");
            mode.className = "editor-modeline";
            panel.append(view, mode);
            paintRows(
                view,
                mode,
                data,
                data.rows || [],
                data.point_line || 0,
                data.point_column || 0,
                selected,
            );
            state.windowElements.set(key, { panel, viewport: view, modeline: mode });
            if (selected) {
                selectedViewport = view;
                selectedModeline = mode;
            }
            return panel;
        }

        frameRoot.replaceChildren(build(snapshot.frame_tree));
        if (selectedViewport) {
            viewport = selectedViewport;
            modeline = selectedModeline;
        }
        scheduleViewportMeasurements();
    }

    function viewportSize(view) {
        const style = win && typeof win.getComputedStyle === "function"
            ? win.getComputedStyle(view)
            : null;
        const paddingY = style
            ? (Number.parseFloat(style.paddingTop) || 0) + (Number.parseFloat(style.paddingBottom) || 0)
            : 0;
        const paddingX = style
            ? (Number.parseFloat(style.paddingLeft) || 0) + (Number.parseFloat(style.paddingRight) || 0)
            : 0;
        const line = view.querySelector(".editor-line");
        const rowHeight = Math.max(1, Number(line && line.offsetHeight) || 18);
        const height = Math.max(1, Math.floor((Number(view.clientHeight) - paddingY) / rowHeight));
        const fontSize = style ? Number.parseFloat(style.fontSize) || 14 : 14;
        const charWidth = Math.max(1, fontSize * 0.6);
        const measuredWidth = Number(view.clientWidth);
        const width = Number.isFinite(measuredWidth) && measuredWidth > 0
            ? Math.max(1, Math.floor((measuredWidth - paddingX) / charWidth))
            : 80;
        return { height, width };
    }

    function reportViewportSizes() {
        viewportMeasurementScheduled = false;
        for (const [key, elements] of state.windowElements) {
            const size = viewportSize(elements.viewport);
            const signature = `${size.height}:${size.width}`;
            if (state.reportedViewportSizes.get(key) === signature) continue;
            state.reportedViewportSizes.set(key, signature);
            send({
                kind: "viewport",
                window: Number(key),
                line_count: size.height,
                height: size.height,
                width: size.width,
            });
        }
    }

    function scheduleViewportMeasurements() {
        if (!measureViewports || viewportMeasurementScheduled) return;
        viewportMeasurementScheduled = true;
        if (win && typeof win.requestAnimationFrame === "function") {
            win.requestAnimationFrame(reportViewportSizes);
        } else {
            Promise.resolve().then(reportViewportSizes);
        }
    }

    function dragRatio(event) {
        if (!draggingSplit) return null;
        const vertical = draggingSplit.axis === "vertical";
        const start = vertical ? draggingSplit.rect.left : draggingSplit.rect.top;
        const size = vertical ? draggingSplit.rect.width : draggingSplit.rect.height;
        const coordinate = vertical ? event.clientX : event.clientY;
        if (!Number.isFinite(size) || size <= 0 || !Number.isFinite(coordinate)) return null;
        return Math.max(1, Math.min(999, Math.round(((coordinate - start) / size) * 1000)));
    }

    function updateDividerDrag(event) {
        const ratio = dragRatio(event);
        if (ratio === null) return;
        event.preventDefault();
        draggingSplit.ratio = ratio;
        draggingSplit.first.style.flexGrow = String(ratio);
        draggingSplit.second.style.flexGrow = String(1000 - ratio);
    }

    function finishDividerDrag(event) {
        if (!draggingSplit) return;
        updateDividerDrag(event);
        const item = {
            kind: "resize_split",
            split: draggingSplit.split,
            ratio: draggingSplit.ratio,
        };
        draggingSplit = null;
        send(item);
    }

    function renderEcho() {
        const snapshot = state.snapshot || {};
        echo.className = state.error ? "editor-echo error" : "editor-echo";
        echo.replaceChildren();
        if (snapshot.minibuffer_active) {
            const prompt = doc.createElement("span");
            prompt.textContent = snapshot.minibuffer_prompt || "";
            const text = doc.createElement("span");
            text.textContent = (snapshot.minibuffer_text || "") + state.provisionalText;
            echo.append(prompt, text, doc.createTextNode("▏"));
            if (state.message) {
                const note = doc.createElement("span");
                note.className = "editor-note";
                note.textContent = "  " + state.message;
                echo.appendChild(note);
            }
            return;
        }
        if (state.pending) {
            const pending = doc.createElement("span");
            pending.className = "pending";
            pending.textContent = state.pending + "-";
            echo.appendChild(pending);
            return;
        }
        if (state.message) echo.textContent = state.message;
    }

    // Moves the hidden input target near the caret so input methods compose in
    // the right place, and keeps the caret visible.
    function placeInputTarget() {
        const caret = viewport.querySelector(".editor-caret");
        if (!caret) return;
        const line = caret.parentElement;
        const top = line.offsetTop;
        const bottom = top + line.offsetHeight;
        if (top < viewport.scrollTop) viewport.scrollTop = top;
        else if (bottom > viewport.scrollTop + viewport.clientHeight) {
            viewport.scrollTop = bottom - viewport.clientHeight;
        }
        inputTarget.style.top = `${top - viewport.scrollTop}px`;
        inputTarget.style.left = `${caret.offsetLeft}px`;
    }

    function keyItem(chord) {
        return { kind: "key", key: chord };
    }

    function textItem(text) {
        return { kind: "text", text };
    }

    function inputItem(inputType, text) {
        return { kind: "input", input_type: inputType, text: text || "" };
    }

    function attachInput() {
        inputTarget.addEventListener("keydown", (event) => {
            if (event.isComposing || state.composing) return;
            if (event.key === "AltGraph") return;
            const chord = chordFor(event, isMac, state.pending !== "");
            if (!chord) return;
            const named = NAMED_KEYS[event.key];
            const modified = event.ctrlKey || event.altKey || (isMac && event.metaKey);
            const printable = event.key.length === 1;
            // Printable text comes from `beforeinput`, unless a prefix is pending:
            // then the key is the next chord of the sequence (C-x 2, C-x o, ...).
            if (printable && !modified && !state.pending) return;
            if (modified || named || printable || SWALLOWED.has(event.key)) {
                event.preventDefault();
                send(keyItem(chord));
            }
        });

        inputTarget.addEventListener("beforeinput", (event) => {
            if (state.composing) return;
            event.preventDefault();
            switch (event.inputType) {
                case "insertText":
                case "insertCompositionText":
                    if (event.data) send(textItem(event.data));
                    break;
                case "insertLineBreak":
                    send(inputItem("insertLineBreak", ""));
                    break;
                case "deleteContentBackward":
                    send(inputItem("deleteContentBackward", ""));
                    break;
                case "deleteContentForward":
                    send(inputItem("deleteContentForward", ""));
                    break;
                default:
                    break;
            }
            inputTarget.value = "";
        });

        inputTarget.addEventListener("paste", (event) => {
            event.preventDefault();
            const text = event.clipboardData ? event.clipboardData.getData("text/plain") : "";
            if (text) send({ kind: "paste", text });
            inputTarget.value = "";
        });

        inputTarget.addEventListener("compositionstart", () => {
            state.composing = true;
            state.composingText = "";
        });
        inputTarget.addEventListener("compositionupdate", (event) => {
            state.composingText = event.data || "";
        });
        inputTarget.addEventListener("compositionend", (event) => {
            state.composing = false;
            const text = event.data || state.composingText;
            state.composingText = "";
            inputTarget.value = "";
            if (text) send(textItem(text));
        });

        frameRoot.addEventListener("mousedown", (event) => {
            event.preventDefault();
            inputTarget.focus();
            if (!event.target || typeof event.target.closest !== "function") return;
            const panel = event.target.closest(".editor-window");
            if (!panel) return;
            const targetWindow = Number(panel.dataset.window);
            const hasTargetWindow = Number.isSafeInteger(targetWindow);
            const targetState = state.windowStates.get(String(targetWindow));
            const targetIsPicker = targetState && targetState.picker === true;
            const line = event.target.closest(".editor-line");
            if (!line || !line._row) {
                if (
                    !targetIsPicker && hasTargetWindow
                    && String(targetWindow) !== String(state.selectedWindow)
                ) {
                    selectWindowLocally(targetWindow);
                    send({ kind: "select_window", window: targetWindow });
                }
                return;
            }
            const row = line._row;
            let utf16 = null;
            if (doc.caretRangeFromPoint) {
                const range = doc.caretRangeFromPoint(event.clientX, event.clientY);
                if (range && line.contains(range.startContainer)) {
                    utf16 = utf16OffsetInLine(line, range.startContainer, range.startOffset);
                }
            } else if (doc.caretPositionFromPoint) {
                const position = doc.caretPositionFromPoint(event.clientX, event.clientY);
                if (position && line.contains(position.offsetNode)) {
                    utf16 = utf16OffsetInLine(line, position.offsetNode, position.offset);
                }
            }
            if (utf16 === null) {
                if (
                    !targetIsPicker && hasTargetWindow
                    && String(targetWindow) !== String(state.selectedWindow)
                ) {
                    selectWindowLocally(targetWindow);
                    send({ kind: "select_window", window: targetWindow });
                }
                return;
            }
            const pointer = {
                kind: "pointer",
                scalar_offset: (row.start || 0) + scalarPrefixLength(row.text || "", utf16),
                extend: event.shiftKey,
            };
            if (line._window !== undefined && line._window !== null) {
                pointer.window = line._window;
            } else if (hasTargetWindow) {
                pointer.window = targetWindow;
            }
            if (!targetIsPicker && pointer.window !== undefined) selectWindowLocally(pointer.window);
            send(pointer);
        });

        // Keystrokes only reach the editor while the hidden input has focus. Click
        // anywhere in the editor to restore it, and never let focus drift away.
        if (doc.addEventListener) {
            doc.addEventListener("mousedown", (event) => {
                if (!root.contains || !root.contains(event.target)) return;
                if (event.target !== inputTarget) event.preventDefault();
                inputTarget.focus();
            });
        }
        if (win && win.addEventListener) {
            win.addEventListener("focus", () => inputTarget.focus());
            win.addEventListener("resize", scheduleViewportMeasurements);
            win.addEventListener("mousemove", updateDividerDrag);
            win.addEventListener("mouseup", finishDividerDrag);
        }
    }

    async function boot() {
        buildChrome();
        attachInput();
        connectEvents();
        try {
            const response = await request(snapshotUrl(), {});
            if (!response.ok) throw new Error(`HTTP ${response.status}`);
            render(await response.json());
        } catch (error) {
            state.message = `cannot reach the editor: ${error}`;
            state.error = true;
            renderEcho();
        }
    }

    boot();

    return {
        state,
        elements: {
            get viewport() {
                return viewport;
            },
            get modeline() {
                return modeline;
            },
            get frameRoot() {
                return frameRoot;
            },
            echo,
            inputTarget,
        },
        chordFor: (event) => chordFor(event, isMac),
        send,
        boot,
    };
}

if (typeof document !== "undefined" && typeof window !== "undefined") {
    // Exposed for DevTools: `__micaEditor.state` and the queued items.
    window.__micaEditor = createEditor({ debug: true });
}
