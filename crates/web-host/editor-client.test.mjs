// Tests for the browser editor client: key normalization and the input paths
// that decide where typed text lands (text items, not swallowed key chords).
//
// Run with: node --test crates/web-host/editor-client.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
    applyEditsToRows,
    chordFor,
    createEditor,
    replayInsertions,
    replayPredictions,
    scalarPrefixLength,
    utf16OffsetForScalar,
} from "./editor-client.js";

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

// --- A DOM small enough to drive the real client event paths ---------------

function makeTextNode(text) {
    return { nodeType: 3, textContent: text, data: text, parentElement: null };
}

function makeElement(tag) {
    const listeners = new Map();
    const element = {
        tagName: tag.toUpperCase(),
        children: [],
        childNodes: [],
        style: {},
        dataset: {},
        className: "",
        value: "",
        parentElement: null,
        offsetTop: 0,
        offsetLeft: 0,
        offsetHeight: 18,
        clientHeight: 400,
        scrollTop: 0,
        addEventListener(type, handler) {
            if (!listeners.has(type)) listeners.set(type, []);
            listeners.get(type).push(handler);
        },
        dispatch(type, event) {
            if (!event.target) event.target = this;
            if (!event.stopPropagation) {
                event.stopPropagation = () => {
                    event.cancelBubble = true;
                };
            }
            for (const handler of listeners.get(type) || []) handler(event);
            if (this.parentElement && !event.cancelBubble) this.parentElement.dispatch(type, event);
        },
        appendChild(child) {
            this._raw = undefined;
            if (child.nodeType !== 3) this.children.push(child);
            this.childNodes.push(child);
            child.parentElement = this;
            return child;
        },
        append(...nodes) {
            for (const node of nodes) this.appendChild(node);
        },
        replaceChildren(...nodes) {
            this.children = [];
            this.childNodes = [];
            for (const node of nodes) this.appendChild(node);
        },
        setAttribute() {},
        focus() {},
        getBoundingClientRect() {
            return this._rect || { left: 0, top: 0, width: 1000, height: 800 };
        },
        querySelector(selector) {
            const wanted = selector.replace(/^\./, "");
            const visit = (node) => {
                for (const child of node.children) {
                    if (child.className.split(" ").includes(wanted)) return child;
                    const found = visit(child);
                    if (found) return found;
                }
                return null;
            };
            return visit(this);
        },
        closest(selector) {
            const wanted = selector.replace(/^\./, "");
            let node = this;
            while (node) {
                if (node.className && node.className.split(" ").includes(wanted)) return node;
                node = node.parentElement;
            }
            return null;
        },
        contains(node) {
            let current = node;
            while (current) {
                if (current === this) return true;
                current = current.parentElement;
            }
            return false;
        },
    };
    Object.defineProperty(element, "textContent", {
        get() {
            if (this._raw !== undefined) return this._raw;
            return this.childNodes.map((child) => child.textContent).join("");
        },
        set(value) {
            this._raw = value;
            this.children = [];
            this.childNodes = [];
        },
    });
    return element;
}

function makeWindow() {
    const listeners = new Map();
    return {
        addEventListener(type, handler) {
            if (!listeners.has(type)) listeners.set(type, []);
            listeners.get(type).push(handler);
        },
        dispatch(type, event = {}) {
            if (!event.preventDefault) event.preventDefault = () => {};
            for (const handler of listeners.get(type) || []) handler(event);
        },
    };
}

function makeDocument(session) {
    const root = makeElement("div");
    root.dataset.session = session;
    const doc = {
        root,
        getElementById: (id) => (id === "mica-editor" ? root : null),
        createElement: (tag) => makeElement(tag),
        createTextNode: (text) => makeTextNode(text),
        caretRangeFromPoint: null,
    };
    return doc;
}

function baseSnapshot(overrides = {}) {
    return {
        session: "41",
        buffer_name: "*scratch*",
        modified: false,
        revision: 1,
        first_line: 0,
        rows: [{ line: 0, start: 0, stop: 0, text: "", complete: true }],
        point: 0,
        point_line: 0,
        point_column: 0,
        mark: null,
        mark_active: false,
        pending: "",
        minibuffer_active: false,
        minibuffer_prompt: "",
        minibuffer_text: "",
        ...overrides,
    };
}

function twoWindowSnapshot(overrides = {}) {
    const first = {
        ...baseSnapshot(),
        window: 1,
        buffer: "editor/buffer/shared",
        rows: [{ line: 0, start: 0, stop: 2, text: "ab", complete: true }],
        point: 1,
        point_column: 1,
    };
    const second = {
        ...first,
        window: 3,
        point: 2,
        point_column: 2,
    };
    return {
        ...first,
        selected_window: 1,
        windows: [first, second],
        frame_tree: {
            kind: "split",
            node: 2,
            axis: "vertical",
            ratio: 500,
            first: { kind: "window", window: 1 },
            second: { kind: "window", window: 3 },
        },
        ...overrides,
    };
}

// A fetch double that records requests and serves queued snapshots.
function makeTransport(initial, responses = []) {
    const requests = [];
    const fetch = async (url, options = {}) => {
        if (options && options.method === "POST") {
            requests.push(JSON.parse(options.body));
            const snapshot = responses.length > 0 ? responses.shift() : initial;
            return { ok: true, json: async () => ({ result: { status: "ok" }, snapshot }) };
        }
        return { ok: true, json: async () => initial };
    };
    return { fetch, requests };
}

function keyEvent(overrides = {}) {
    return {
        key: "",
        isComposing: false,
        ctrlKey: false,
        altKey: false,
        shiftKey: false,
        metaKey: false,
        canceled: false,
        preventDefault() {
            this.canceled = true;
        },
        ...overrides,
    };
}

async function installEditor(snapshot, transport, options = {}) {
    const doc = makeDocument("41");
    const editor = createEditor({
        document: doc,
        window: {},
        navigator: { platform: "Linux" },
        fetch: transport.fetch,
        root: doc.root,
        measureViewports: false,
        ...options,
    });
    await tick();
    return { editor, doc };
}

class FakeEventSource {
    static instances = [];

    constructor(url) {
        this.url = url;
        this.listeners = new Map();
        FakeEventSource.instances.push(this);
    }

    addEventListener(type, handler) {
        if (!this.listeners.has(type)) this.listeners.set(type, []);
        this.listeners.get(type).push(handler);
    }

    emit(type, data = "") {
        for (const handler of this.listeners.get(type) || []) handler({ data });
    }

    close() {
        this.closed = true;
    }
}

// --- Pure normalization -----------------------------------------------------

test("chords normalize modifier order and named keys", () => {
    assert.equal(chordFor(keyEvent({ key: "f", ctrlKey: true }), false), "C-f");
    assert.equal(chordFor(keyEvent({ key: "F", ctrlKey: true, shiftKey: true }), false), "C-S-f");
    assert.equal(chordFor(keyEvent({ key: "ArrowLeft" }), false), "<left>");
    assert.equal(chordFor(keyEvent({ key: "Enter" }), false), "<return>");
    assert.equal(chordFor(keyEvent({ key: "x", altKey: true }), false), "M-x");
    assert.equal(chordFor(keyEvent({ key: "x", metaKey: true }), true), "s-x");
});

test("plain printable keys are not chords", () => {
    assert.equal(chordFor(keyEvent({ key: "a" }), false), null);
    assert.equal(chordFor(keyEvent({ key: "A", shiftKey: true }), false), "S-a");
    assert.equal(chordFor(keyEvent({ key: " " }), false), "<space>");
});

test("scalar prefix length converts UTF-16 offsets", () => {
    assert.equal(scalarPrefixLength("hello", 3), 3);
    assert.equal(scalarPrefixLength("h\u00e9llo", 2), 2);
    // An emoji is two UTF-16 units and one scalar.
    assert.equal(scalarPrefixLength("a\ud83d\ude00b", 3), 2);
    assert.equal(scalarPrefixLength("a\ud83d\ude00b", 4), 3);
});

test("scalar columns convert back to UTF-16 offsets", () => {
    assert.equal(utf16OffsetForScalar("a\ud83d\ude00b", 0), 0);
    assert.equal(utf16OffsetForScalar("a\ud83d\ude00b", 2), 3);
    assert.equal(utf16OffsetForScalar("a\ud83d\ude00b", 3), 4);
});

test("replay applies queued insertions to a copy", () => {
    const rows = [{ line: 0, start: 0, stop: 2, text: "ab", complete: true }];
    const replayed = replayInsertions(rows, 0, 1, ["X", "Y"]);
    assert.equal(replayed.rows[0].text, "aXYb");
    assert.equal(replayed.line, 0);
    assert.equal(replayed.column, 3);
    assert.equal(rows[0].text, "ab", "the authoritative rows are not mutated");
});

test("replay splits inserted newlines into lines", () => {
    const rows = [{ line: 9, start: 20, stop: 22, text: "ab", complete: true }];
    const replayed = replayInsertions(rows, 9, 1, ["\n"]);
    assert.equal(replayed.rows[0].text, "a");
    assert.equal(replayed.rows[1].text, "b");
    assert.equal(replayed.rows[0].line, 9);
    assert.equal(replayed.rows[1].line, 10);
    assert.equal(replayed.line, 10);
    assert.equal(replayed.column, 0);
});

test("replay uses scalar columns around astral characters", () => {
    const rows = [{ line: 0, start: 0, stop: 3, text: "a\ud83d\ude00b", complete: true }];
    const replayed = replayInsertions(rows, 0, 2, ["X"]);
    assert.equal(replayed.rows[0].text, "a\ud83d\ude00Xb");
    assert.equal(replayed.column, 3);
});

test("authoritative edits update a scalar-indexed viewport", () => {
    const rows = [
        { line: 0, start: 0, stop: 2, text: "ab", complete: true },
        { line: 1, start: 3, stop: 5, text: "cd", complete: true },
    ];
    const joined = applyEditsToRows(rows, [{ at: 2, remove: 1, text: "" }]);
    assert.equal(joined.ok, true);
    assert.deepEqual(joined.rows.map((row) => row.text), ["abcd"]);
    assert.equal(joined.rows[0].stop, 4);

    const unicode = applyEditsToRows(joined.rows, [{ at: 1, remove: 1, text: "😀\n" }]);
    assert.equal(unicode.ok, true);
    assert.deepEqual(unicode.rows.map((row) => row.text), ["a😀", "cd"]);
    assert.equal(unicode.rows[0].stop, 2, "the emoji is one scalar");
    assert.equal(unicode.rows[1].start, 3);
});

test("predictors replay mixed edits, movement, and mark commands", () => {
    const rows = [
        { line: 0, start: 0, stop: 2, text: "ab", complete: true },
        { line: 1, start: 3, stop: 5, text: "cd", complete: true },
    ];
    const replayed = replayPredictions(rows, 0, 2, [
        { predictor: "insert_text", item: { kind: "key", key: "<return>" }, barrier: false },
        { predictor: "insert_text", item: { kind: "text", text: "X" }, barrier: false },
        { predictor: "move_logical_line_down", item: { kind: "key", key: "C-n" }, barrier: false },
        { predictor: "move_forward_scalar", item: { kind: "key", key: "C-f" }, barrier: false },
        { predictor: "delete_backward_scalar", item: { kind: "key", key: "<backspace>" }, barrier: false },
        { predictor: "set_mark", item: { kind: "key", key: "C-<space>" }, barrier: false },
    ]);
    assert.equal(replayed.complete, true);
    assert.deepEqual(replayed.rows.map((row) => row.text), ["ab", "X", "c"]);
    assert.equal(replayed.line, 2);
    assert.equal(replayed.column, 1);
    assert.equal(replayed.markActive, true);
    assert.equal(replayed.mark, 6);
});

// --- Input paths ------------------------------------------------------------

test("plain letters are sent as text items, not key chords", async () => {
    const transport = makeTransport(baseSnapshot());
    const { editor } = await installEditor(baseSnapshot(), transport);
    const input = editor.elements.inputTarget;

    input.dispatch("keydown", keyEvent({ key: "a" }));
    await tick();
    assert.equal(transport.requests.length, 0, "keydown for a letter sends nothing");

    input.dispatch("beforeinput", {
        inputType: "insertText",
        data: "a",
        preventDefault() {},
    });
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "text", text: "a" });
});

test("space and capitals go through beforeinput", async () => {
    const transport = makeTransport(baseSnapshot());
    const { editor } = await installEditor(baseSnapshot(), transport);
    const input = editor.elements.inputTarget;

    input.dispatch("keydown", keyEvent({ key: " " }));
    await tick();
    assert.equal(transport.requests.length, 0, "space keydown sends nothing");

    input.dispatch("beforeinput", { inputType: "insertText", data: " ", preventDefault() {} });
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "text", text: " " });

    input.dispatch("keydown", keyEvent({ key: "A", shiftKey: true }));
    await tick();
    assert.equal(transport.requests.length, 1, "shift-A keydown sends nothing");

    input.dispatch("beforeinput", { inputType: "insertText", data: "A", preventDefault() {} });
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "text", text: "A" });
});

test("control chords and named keys are sent as key items", async () => {
    const transport = makeTransport(baseSnapshot());
    const { editor } = await installEditor(baseSnapshot(), transport);
    const input = editor.elements.inputTarget;

    input.dispatch("keydown", keyEvent({ key: "f", ctrlKey: true }));
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "key", key: "C-f" });

    input.dispatch("keydown", keyEvent({ key: "Enter" }));
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "key", key: "<return>" });

    input.dispatch("keydown", keyEvent({ key: "Backspace" }));
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "key", key: "<backspace>" });
});

test("100 ms RTT does not serialize rapid typing, movement, deletion, and newline", async () => {
    FakeEventSource.instances = [];
    const snapshot = baseSnapshot({
        keymap_generation: 7,
        keymap_plan: [
            { sequence: "C-b", predictor: "move_backward_scalar", barrier: false },
            { sequence: "<return>", predictor: "insert_text", barrier: false },
        ],
    });
    const batches = [];
    let accepted = 0;
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => snapshot };
            batches.push(JSON.parse(options.body));
            return new Promise((resolve) =>
                setTimeout(() => {
                    accepted += 1;
                    resolve({ ok: true, json: async () => ({ accepted: true }) });
                }, 100)
            );
        },
    };
    const { editor } = await installEditor(snapshot, transport, { EventSource: FakeEventSource });
    const input = editor.elements.inputTarget;
    FakeEventSource.instances[0].emit("open");

    input.dispatch("beforeinput", { inputType: "insertText", data: "a", preventDefault() {} });
    input.dispatch("beforeinput", { inputType: "insertText", data: "b", preventDefault() {} });
    await tick();
    assert.equal(batches.length, 1);
    assert.equal(accepted, 0, "the first 100 ms admission is still in flight");

    input.dispatch("keydown", keyEvent({ key: "b", ctrlKey: true }));
    input.dispatch("beforeinput", { inputType: "deleteContentForward", data: null, preventDefault() {} });
    input.dispatch("beforeinput", { inputType: "insertLineBreak", data: null, preventDefault() {} });
    await tick();

    assert.equal(batches.length, 2, "a second batch starts before the first response");
    assert.equal(accepted, 0);
    assert.equal(editor.state.pointLine, 1);
    assert.equal(editor.state.pointColumn, 0);
    assert.equal(editor.elements.viewport.children.length, 2);
    assert.equal(editor.elements.viewport.children[0].textContent, "a");
    assert.deepEqual(
        batches.flatMap((batch) => batch.items.map((item) => item.sequence)),
        ["1", "2", "3", "4", "5"],
    );
});

test("SSE results advance the replica in sequence order", async () => {
    FakeEventSource.instances = [];
    const snapshot = baseSnapshot({ keymap_generation: 4, keymap_plan: [] });
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => snapshot };
            return { ok: true, json: async () => ({ accepted: true }) };
        },
    };
    const { editor } = await installEditor(snapshot, transport, { EventSource: FakeEventSource });
    const input = editor.elements.inputTarget;
    const source = FakeEventSource.instances[0];
    source.emit("open");
    input.dispatch("beforeinput", { inputType: "insertText", data: "a", preventDefault() {} });
    input.dispatch("beforeinput", { inputType: "insertText", data: "b", preventDefault() {} });
    await tick();

    const result = (sequence, at, text, point) =>
        JSON.stringify({
            through_sequence: sequence,
            result: {
                status: "ok",
                edits: [{ at, remove: 0, text }],
                point,
                point_line: 0,
                point_column: point,
                mark: point,
                mark_active: false,
                first_line: 0,
                pending: "",
                buffer_name: "*scratch*",
                modified: true,
            },
        });
    source.emit("editor", result(2, 1, "b", 2));
    assert.equal(editor.state.baseRows[0].text, "", "sequence two waits for sequence one");
    source.emit("editor", result(1, 0, "a", 1));
    assert.equal(editor.state.baseRows[0].text, "ab");
    assert.equal(editor.state.throughSequence, 2);
    assert.equal(editor.state.outstanding.length, 0);
});

test("scroll commands update the viewport before the server result", async () => {
    const snapshot = baseSnapshot({
        keymap_plan: [
            { sequence: "C-v", predictor: "scroll_lines", barrier: true },
            { sequence: "M-v", predictor: "scroll_lines", barrier: true },
        ],
    });
    let resolveInput;
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => snapshot };
            return new Promise((resolve) => {
                resolveInput = () =>
                    resolve({ ok: true, json: async () => ({ result: { status: "resync" }, snapshot }) });
            });
        },
    };
    const { editor } = await installEditor(snapshot, transport);
    editor.elements.inputTarget.dispatch("keydown", keyEvent({ key: "v", ctrlKey: true }));
    assert.equal(editor.elements.viewport.scrollTop, 400);
    resolveInput();
    await tick();
});

test("a printable key completes a pending prefix as a chord", async () => {
    const snapshot = baseSnapshot({ pending: "C-x" });
    const transport = makeTransport(snapshot);
    const { editor } = await installEditor(snapshot, transport);
    const input = editor.elements.inputTarget;

    assert.equal(editor.state.pending, "C-x", "the client tracks the server's pending sequence");

    input.dispatch("keydown", keyEvent({ key: "2" }));
    await tick();
    assert.deepEqual(transport.requests.at(-1), { kind: "key", key: "2" });
});

test("a fast printable key completes an in-flight prefix", async () => {
    const initial = baseSnapshot({
        keymap_plan: [{ sequence: "C-x 2" }, { sequence: "C-x o" }],
    });
    let resolvePrefix;
    const requests = [];
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => initial };
            requests.push(JSON.parse(options.body));
            if (requests.length === 1) {
                return new Promise((resolve) => {
                    resolvePrefix = () =>
                        resolve({
                            ok: true,
                            json: async () => ({
                                result: { status: "prefix" },
                                snapshot: { ...initial, pending: "C-x" },
                            }),
                        });
                });
            }
            return { ok: true, json: async () => ({ result: { status: "ok" }, snapshot: initial }) };
        },
    };
    const { editor } = await installEditor(initial, transport);
    const input = editor.elements.inputTarget;
    input.dispatch("keydown", keyEvent({ key: "x", ctrlKey: true }));
    input.dispatch("keydown", keyEvent({ key: "2" }));
    assert.equal(editor.state.queue[0].item.kind, "key");
    assert.equal(editor.state.queue[0].item.key, "2");
    resolvePrefix();
    await tick();
    await tick();
    await tick();
    assert.deepEqual(requests, [{ kind: "key", key: "C-x" }, { kind: "key", key: "2" }]);
});

test("an ambiguous failure retries the same sequenced item", async () => {
    const initial = baseSnapshot();
    const postUrls = [];
    let attempts = 0;
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => initial };
            attempts += 1;
            postUrls.push(url);
            if (attempts === 1) throw new Error("lost response");
            return { ok: true, json: async () => ({ result: { status: "ok" }, snapshot: initial }) };
        },
    };
    const { editor } = await installEditor(initial, transport);
    editor.elements.inputTarget.dispatch("beforeinput", {
        inputType: "insertText",
        data: "x",
        preventDefault() {},
    });
    await tick();
    await tick();
    await tick();
    assert.equal(attempts, 2);
    assert.match(postUrls[0], /sequence=1/);
    assert.equal(postUrls[0], postUrls[1]);
});

test("the frame tree renders every visible window", async () => {
    const first = { ...baseSnapshot(), window: 1, buffer_name: "one" };
    const second = {
        ...baseSnapshot(),
        window: 3,
        buffer_name: "two",
        rows: [{ line: 0, start: 0, stop: 3, text: "two", complete: true }],
    };
    const snapshot = {
        ...second,
        selected_window: 3,
        windows: [first, second],
        frame_tree: {
            kind: "split",
            axis: "vertical",
            ratio: 500,
            first: { kind: "window", window: 1 },
            second: { kind: "window", window: 3 },
        },
    };
    const transport = makeTransport(snapshot);
    const { editor } = await installEditor(snapshot, transport);
    const split = editor.elements.frameRoot.children[0];
    assert.equal(split.className, "editor-split vertical");
    assert.equal(split.children.length, 3);
    assert.equal(split.children[1].className, "editor-divider vertical");
    assert.ok(editor.elements.frameRoot.textContent.includes("one"));
    assert.ok(editor.elements.frameRoot.textContent.includes("two"));
});

test("only the selected editor window paints a cursor", async () => {
    const snapshot = twoWindowSnapshot({ selected_window: 3, window: 3, point: 2, point_column: 2 });
    const transport = makeTransport(snapshot);
    const { editor } = await installEditor(snapshot, transport);
    const split = editor.elements.frameRoot.children[0];
    const firstPanel = split.children[0];
    const secondPanel = split.children[2];
    assert.equal(firstPanel.querySelector(".editor-caret"), null);
    assert.ok(secondPanel.querySelector(".editor-caret"));
    assert.equal(firstPanel.className, "editor-window");
    assert.equal(secondPanel.className, "editor-window selected");
});

test("clicking inactive window chrome selects that window", async () => {
    const snapshot = twoWindowSnapshot();
    const selected = twoWindowSnapshot({
        selected_window: 3,
        window: 3,
        point: 2,
        point_column: 2,
    });
    const transport = makeTransport(snapshot, [selected]);
    const { editor } = await installEditor(snapshot, transport);
    const split = editor.elements.frameRoot.children[0];
    const firstPanel = split.children[0];
    const secondPanel = split.children[2];

    secondPanel.children[1].dispatch("mousedown", { preventDefault() {} });
    assert.equal(firstPanel.className, "editor-window");
    assert.equal(secondPanel.className, "editor-window selected");
    await tick();

    assert.deepEqual(transport.requests.at(-1), {
        kind: "select_window",
        window: 3,
    });
    const settledSplit = editor.elements.frameRoot.children[0];
    const settledFirstPanel = settledSplit.children[0];
    const settledSecondPanel = settledSplit.children[2];
    assert.equal(settledFirstPanel.querySelector(".editor-caret"), null);
    assert.ok(settledSecondPanel.querySelector(".editor-caret"));
});

test("clicking a picker row leaves the origin window selected", async () => {
    const snapshot = twoWindowSnapshot();
    snapshot.windows[1] = {
        ...snapshot.windows[1],
        buffer: "editor/buffer/completions",
        buffer_name: "*Completions*",
        picker: true,
        rows: [
            { line: 0, start: 0, stop: 7, text: "> first", complete: true },
            { line: 1, start: 8, stop: 16, text: "  second", complete: true },
        ],
        point: 2,
        point_line: 0,
        point_column: 2,
    };
    const transport = makeTransport(snapshot);
    const { editor, doc } = await installEditor(snapshot, transport);
    const split = editor.elements.frameRoot.children[0];
    const firstPanel = split.children[0];
    const pickerPanel = split.children[2];
    const secondLine = pickerPanel.children[0].children[1];
    assert.equal(secondLine._row.line, 1);
    assert.equal(secondLine.closest(".editor-window"), pickerPanel);
    doc.caretRangeFromPoint = () => ({
        startContainer: secondLine,
        startOffset: 0,
    });

    secondLine.dispatch("mousedown", {
        clientX: 10,
        clientY: 30,
        shiftKey: false,
        preventDefault() {},
    });
    assert.equal(firstPanel.className, "editor-window selected");
    assert.equal(pickerPanel.className, "editor-window");
    await tick();

    assert.deepEqual(transport.requests.at(-1), {
        kind: "pointer",
        scalar_offset: 16,
        extend: false,
        window: 3,
    });
});

test("the browser reports each rendered window height", async () => {
    const snapshot = twoWindowSnapshot();
    const transport = makeTransport(snapshot);
    const window = makeWindow();
    let measure = null;
    window.requestAnimationFrame = (callback) => {
        measure = callback;
    };
    const { editor } = await installEditor(snapshot, transport, {
        window,
        measureViewports: true,
    });
    const split = editor.elements.frameRoot.children[0];
    split.children[0].children[0].clientHeight = 180;
    split.children[2].children[0].clientHeight = 252;
    assert.ok(measure, "a measurement is scheduled after frame rendering");
    measure();
    await tick();
    await tick();

    assert.deepEqual(transport.requests.slice(0, 2), [
        { kind: "viewport", window: 1, line_count: 10, height: 10, width: 80 },
        { kind: "viewport", window: 3, line_count: 14, height: 14, width: 80 },
    ]);
});

test("an edit updates every visible window on the same buffer", async () => {
    const snapshot = twoWindowSnapshot();
    let resolveInput;
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => snapshot };
            return new Promise((resolve) => {
                resolveInput = () =>
                    resolve({
                        ok: true,
                        json: async () => ({
                            through_sequence: 1,
                            result: {
                                status: "ok",
                                buffer: "editor/buffer/shared",
                                revision: 2,
                                edits: [{ at: 1, remove: 0, text: "X" }],
                                window: 1,
                                selected_window: 1,
                                point: 2,
                                point_line: 0,
                                point_column: 2,
                                mark: 2,
                                mark_active: false,
                                first_line: 0,
                                pending: "",
                                windows: [
                                    { ...snapshot.windows[0], revision: 2, point: 2, point_column: 2 },
                                    { ...snapshot.windows[1], revision: 2, point: 3, point_column: 3 },
                                ],
                            },
                        }),
                    });
            });
        },
    };
    const { editor } = await installEditor(snapshot, transport);
    const input = editor.elements.inputTarget;
    input.dispatch("beforeinput", { inputType: "insertText", data: "X", preventDefault() {} });
    await tick();
    let split = editor.elements.frameRoot.children[0];
    assert.equal(split.children[0].children[0].textContent, "aXb");
    assert.equal(split.children[2].children[0].textContent, "aXb");
    assert.equal(split.children[2].querySelector(".editor-caret"), null);

    resolveInput();
    await tick();
    split = editor.elements.frameRoot.children[0];
    assert.equal(split.children[0].children[0].textContent, "aXb");
    assert.equal(split.children[2].children[0].textContent, "aXb");
    assert.equal(editor.state.windowStates.get("3").point, 3);
});

test("dragging a divider publishes one authoritative split ratio", async () => {
    const snapshot = twoWindowSnapshot();
    const transport = makeTransport(snapshot);
    const window = makeWindow();
    const { editor } = await installEditor(snapshot, transport, { window });
    const split = editor.elements.frameRoot.children[0];
    const divider = split.children[1];
    divider.dispatch("mousedown", {
        clientX: 500,
        clientY: 0,
        preventDefault() {},
    });
    window.dispatch("mousemove", { clientX: 750, clientY: 0 });
    assert.equal(split.children[0].style.flexGrow, "750");
    assert.equal(split.children[2].style.flexGrow, "250");
    window.dispatch("mouseup", { clientX: 750, clientY: 0 });
    await tick();
    assert.deepEqual(transport.requests.at(-1), {
        kind: "resize_split",
        split: 2,
        ratio: 750,
    });
});

test("typing paints provisionally before the result arrives", async () => {
    const authoritative = baseSnapshot({
        revision: 1,
        rows: [{ line: 0, start: 0, stop: 1, text: "a", complete: true }],
        point: 1,
        point_column: 1,
    });
    let resolveInput;
    const requests = [];
    const transport = {
        fetch: async (url, options = {}) => {
            if (options && options.method === "POST") {
                requests.push(JSON.parse(options.body));
                return new Promise((resolve) => {
                    resolveInput = () =>
                        resolve({
                            ok: true,
                            json: async () => ({ result: { status: "ok" }, snapshot: authoritative }),
                        });
                });
            }
            return { ok: true, json: async () => baseSnapshot() };
        },
    };
    const { editor } = await installEditor(baseSnapshot(), transport);
    const input = editor.elements.inputTarget;

    input.dispatch("beforeinput", { inputType: "insertText", data: "a", preventDefault() {} });
    await tick();
    assert.equal(editor.elements.viewport.textContent, "a", "the character is painted before the result");
    assert.equal(editor.state.pointColumn, 1, "the provisional caret advanced");

    resolveInput();
    await tick();
    assert.equal(editor.elements.viewport.textContent, "a", "the authoritative render agrees");
    assert.equal(requests.length, 1, "the item is sent exactly once");
});

test("a compact result advances the authoritative replica without rebuilding its line", async () => {
    let resolveInput;
    const transport = {
        fetch: async (url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => baseSnapshot() };
            return new Promise((resolve) => {
                resolveInput = () =>
                    resolve({
                        ok: true,
                        json: async () => ({
                            through_sequence: 1,
                            result: {
                                status: "ok",
                                revision: 2,
                                edits: [{ at: 0, remove: 0, text: "a" }],
                                point: 1,
                                point_line: 0,
                                point_column: 1,
                                mark: 1,
                                mark_active: false,
                                first_line: 0,
                                pending: "",
                                buffer_name: "*scratch*",
                                modified: true,
                                minibuffer_active: false,
                                minibuffer_prompt: "",
                                minibuffer_text: "",
                            },
                        }),
                    });
            });
        },
    };
    const { editor } = await installEditor(baseSnapshot(), transport);
    editor.elements.inputTarget.dispatch("beforeinput", {
        inputType: "insertText",
        data: "a",
        preventDefault() {},
    });
    await tick();
    const provisionalLine = editor.elements.viewport.querySelector(".editor-line");
    resolveInput();
    await tick();
    const authoritativeLine = editor.elements.viewport.querySelector(".editor-line");
    assert.equal(editor.state.baseRows[0].text, "a");
    assert.equal(editor.state.basePoint.column, 1);
    assert.equal(authoritativeLine, provisionalLine, "the existing line node is retained");
});

test("typing at a prompt echoes provisionally without touching the buffer", async () => {
    const promptSnapshot = baseSnapshot({
        minibuffer_active: true,
        minibuffer_prompt: "M-x ",
        minibuffer_text: "",
    });
    let resolveInput;
    const transport = {
        fetch: async (url, options = {}) => {
            if (options && options.method === "POST") {
                return new Promise((resolve) => {
                    resolveInput = () =>
                        resolve({
                            ok: true,
                            json: async () => ({
                                result: { status: "ok" },
                                snapshot: { ...promptSnapshot, minibuffer_text: "e" },
                            }),
                        });
                });
            }
            return { ok: true, json: async () => promptSnapshot };
        },
    };
    const { editor } = await installEditor(promptSnapshot, transport);
    const input = editor.elements.inputTarget;

    input.dispatch("beforeinput", { inputType: "insertText", data: "e", preventDefault() {} });
    await tick();
    assert.equal(editor.elements.viewport.textContent, "", "the buffer is untouched");
    assert.ok(editor.elements.echo.textContent.includes("M-x e"), "the prompt echoes the key");

    resolveInput();
    await tick();
    assert.ok(editor.elements.echo.textContent.includes("M-x e"), "the authoritative echo agrees");
});

test("a prompt result message stays visible while the prompt is open", async () => {
    const promptSnapshot = baseSnapshot({
        minibuffer_active: true,
        minibuffer_prompt: "M-x ",
        minibuffer_text: "bogus",
    });
    const transport = {
        fetch: async (url, options = {}) => {
            if (options && options.method === "POST") {
                return {
                    ok: true,
                    json: async () => ({
                        result: { status: "ok", message: "No match: bogus (C-g or Esc cancels)" },
                        snapshot: promptSnapshot,
                    }),
                };
            }
            return { ok: true, json: async () => promptSnapshot };
        },
    };
    const { editor } = await installEditor(promptSnapshot, transport);
    editor.elements.inputTarget.dispatch("keydown", keyEvent({ key: "Enter" }));
    await tick();
    const echoText = editor.elements.echo.textContent;
    assert.ok(echoText.includes("No match"), `the message is visible: ${echoText}`);
    assert.ok(echoText.includes("C-g"), "the escape hint is visible");
});

test("a click sends a pointer item with the clicked scalar offset", async () => {
    const snapshot = baseSnapshot({
        rows: [{ line: 0, start: 0, stop: 5, text: "hello", complete: true }],
        point_column: 5,
        point: 5,
    });
    const transport = makeTransport(snapshot);
    const { editor, doc } = await installEditor(snapshot, transport);
    const viewport = editor.elements.viewport;
    const line = viewport.querySelector(".editor-line");
    assert.ok(line, "the rendered line exists");
    const textNode = line.childNodes[0];
    doc.caretRangeFromPoint = () => ({ startContainer: textNode, startOffset: 3 });

    viewport.dispatch("mousedown", {
        clientX: 10,
        clientY: 10,
        shiftKey: false,
        target: line,
        preventDefault() {},
    });
    await tick();
    assert.deepEqual(transport.requests.at(-1), {
        kind: "pointer",
        scalar_offset: 3,
        extend: false,
    });
});

test("shift-click extends the region", async () => {
    const snapshot = baseSnapshot({
        rows: [{ line: 0, start: 0, stop: 5, text: "hello", complete: true }],
        point_column: 5,
        point: 5,
    });
    const transport = makeTransport(snapshot);
    const { editor, doc } = await installEditor(snapshot, transport);
    const line = editor.elements.viewport.querySelector(".editor-line");
    doc.caretRangeFromPoint = () => ({ startContainer: line.childNodes[0], startOffset: 1 });

    editor.elements.viewport.dispatch("mousedown", {
        clientX: 5,
        clientY: 5,
        shiftKey: true,
        target: line,
        preventDefault() {},
    });
    await tick();
    assert.deepEqual(transport.requests.at(-1), {
        kind: "pointer",
        scalar_offset: 1,
        extend: true,
    });
});

test("expired asynchronous replies stop retrying and retain outstanding input", async () => {
    FakeEventSource.instances = [];
    const snapshot = baseSnapshot();
    let attempts = 0;
    const transport = {
        fetch: async (_url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => snapshot };
            attempts += 1;
            return { ok: false, status: 410 };
        },
    };
    const { editor } = await installEditor(snapshot, transport, { EventSource: FakeEventSource });
    editor.send({ kind: "text", text: "é🦀" });
    await tick();
    assert.equal(editor.state.blocked, true);
    assert.equal(editor.state.outstanding[0].item.text, "é🦀");
    assert.equal(FakeEventSource.instances[0].closed, true);
    assert.match(editor.state.message, /HTTP 410/);
    editor.send({ kind: "text", text: "later" });
    FakeEventSource.instances[0].emit("open");
    await new Promise((resolve) => setTimeout(resolve, 75));
    assert.equal(attempts, 1);
});

test("denied synchronous input stops retrying and retains the unconfirmed item", async () => {
    const snapshot = baseSnapshot();
    let attempts = 0;
    const transport = {
        fetch: async (_url, options = {}) => {
            if (!options.method) return { ok: true, json: async () => snapshot };
            attempts += 1;
            return { ok: false, status: 403 };
        },
    };
    const { editor } = await installEditor(snapshot, transport, { EventSource: false });
    editor.send({ kind: "text", text: "unconfirmed" });
    await tick();
    assert.equal(editor.state.blocked, true);
    assert.equal(editor.state.inFlightItem.item.text, "unconfirmed");
    editor.send({ kind: "text", text: "later" });
    await new Promise((resolve) => setTimeout(resolve, 75));
    assert.equal(attempts, 1);
});
