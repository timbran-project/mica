// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::builtin::RuntimePorts;
use crate::metrics::RelationOperation;
use crate::program::{CompactListItem, CompactMapItem, CompactRelationArg, Opcode, OperandRef};
#[cfg(feature = "cranelift")]
use crate::program::{MAX_NATURAL_LOOP_COLLECTION_VIEWS, MAX_NATURAL_LOOP_SLOTS};
use crate::{
    AuthorityContext, BuiltinRegistry, CatchHandler, ClientBuiltinContext, ClientBuiltinRegistry,
    Emission, ErrorField, ExternalRequest, KindCheckSite, MailboxRecvRequest, Program,
    ProgramResolver, QueryBinding, Register, RuntimeBinaryOp, RuntimeContext, RuntimeError,
    RuntimeUnaryOp, SpawnRequest, SpawnTarget, SuspendKind, TypeContract, TypeLiteralContract,
};
use mica_relation_kernel::{
    ApplicableMethodCall, DispatchRead, DispatchRelations, RelationId, RelationMetadata,
    RelationRead, RelationWorkspace, ScanControl, Transaction, Tuple,
    applicable_method_calls_normalized, applicable_positional_methods_cached, method_program_id,
    normalize_dispatch_roles, system_row_source_relation,
};
#[cfg(feature = "cranelift")]
use mica_var::ValueRef;
#[cfg(feature = "cranelift")]
use mica_var::abi::{
    borrowed_value_bits, clone_value_bits, from_owned_value_bits, value_is_immediate,
};
use mica_var::{FunctionId, Identity, RelationValue, Symbol, Value, ValueKind};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[cfg(feature = "cranelift")]
use mica_vm_cranelift::{
    FloatArithmetic, FloatComparison, FloatLoopOutcome, FloatLoopPlan, IntegerComparison,
    IntegerLoopOutcome, NaturalLoopCollectionView, NaturalLoopOutcome,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    program: usize,
    ip: usize,
    registers: Vec<Value>,
    return_register: Option<Register>,
    try_stack: Vec<TryRegion>,
    pending_finally: Vec<FinallyContinuation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TryRegion {
    catches: Vec<CatchHandler>,
    finally: Option<usize>,
    end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FinallyContinuation {
    Normal(usize),
    Raise(Value),
    Return(Value),
}

enum ExpandedRelationArg {
    Value(Value),
    Query(Symbol),
    Hole,
}

impl Frame {
    fn root(program: usize, register_count: usize) -> Self {
        Self::new(program, register_count, None, Vec::new()).expect("root frame has no arguments")
    }

    fn new(
        program: usize,
        register_count: usize,
        return_register: Option<Register>,
        args: Vec<Value>,
    ) -> Result<Self, RuntimeError> {
        if args.len() > register_count {
            return Err(RuntimeError::InvalidCallArity {
                expected_min: 0,
                expected_max: register_count,
                actual: args.len(),
            });
        }

        let mut registers = vec![Value::nothing(); register_count];
        for (slot, arg) in registers.iter_mut().zip(args) {
            *slot = arg;
        }
        Ok(Self {
            program,
            ip: 0,
            registers,
            return_register,
            try_stack: Vec::new(),
            pending_finally: Vec::new(),
        })
    }

    pub fn program_index(&self) -> usize {
        self.program
    }

    pub fn ip(&self) -> usize {
        self.ip
    }

    pub fn registers(&self) -> &[Value] {
        &self.registers
    }

    pub fn return_register(&self) -> Option<Register> {
        self.return_register
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmState {
    programs: Vec<Arc<Program>>,
    functions: Vec<CallableInfo>,
    resolved_programs: Vec<(Value, usize)>,
    frames: Vec<Frame>,
    pending_resume: Option<Register>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CallableInfo {
    program: usize,
    captures: Vec<Value>,
    min_arity: usize,
    max_arity: Option<usize>,
}

impl VmState {
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    pub fn current_frame(&self) -> Option<&Frame> {
        self.frames.last()
    }

    pub fn ip(&self) -> usize {
        self.current_frame().map_or(0, Frame::ip)
    }

    pub fn registers(&self) -> &[Value] {
        self.current_frame().map_or(&[], Frame::registers)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VmHostResponse {
    Continue,
    Commit,
    Suspend(SuspendKind),
    Spawn(SpawnRequest),
    Complete(Value),
    Abort(Value),
    RollbackRetry,
}

pub trait VmHost: RelationWorkspace + DispatchRead {
    fn authority(&self) -> &AuthorityContext;

    fn authority_mut(&mut self) -> &mut AuthorityContext;

    fn emit(&mut self, target: Identity, value: Value) -> Result<(), RuntimeError>;

    fn validate_mailbox_receiver(&mut self, receiver: &Value) -> Result<(), RuntimeError>;

    fn resolve_program(
        &mut self,
        program_bytes_relation: mica_relation_kernel::RelationId,
        program_id: &Value,
    ) -> Result<Arc<Program>, RuntimeError>;

    fn call_builtin(&mut self, name: Symbol, args: &[Value]) -> Result<Value, RuntimeError>;
}

pub struct VmHostContext<'ctx, 'kernel> {
    tx: &'ctx mut Transaction<'kernel>,
    authority: &'ctx mut AuthorityContext,
    resolver: &'ctx ProgramResolver,
    builtins: &'ctx BuiltinRegistry,
    ports: RuntimePorts<'ctx>,
    task_snapshot: &'ctx [Value],
    runtime_context: RuntimeContext,
    trace: VmHostTrace,
}

const BUDGET_PROFILE_SAMPLE_INTERVAL: usize = 1024;
const BUDGET_PROFILE_TOP_LIMIT: usize = 12;
#[cfg(feature = "cranelift")]
const MIN_NATIVE_INTEGER_LOOP_ITERATIONS: usize = 4_096;
#[cfg(feature = "cranelift")]
// Cold collection-loop benchmarks win at 8,192 iterations across ranges, lists, and maps.
const MIN_NATIVE_COLLECTION_LOOP_ITERATIONS: usize = 8_192;
#[cfg(feature = "cranelift")]
const MIN_NATIVE_FLOAT_LOOP_ITERATIONS: usize = 8_192;

#[derive(Clone, Debug, Default)]
struct BudgetProfiler {
    samples: BTreeMap<(usize, usize, &'static str), usize>,
}

impl BudgetProfiler {
    fn new() -> Self {
        Self::default()
    }

    fn sample(&mut self, instruction: usize, vm: &RegisterVm) {
        if !instruction.is_multiple_of(BUDGET_PROFILE_SAMPLE_INTERVAL) {
            return;
        }

        let Some(frame) = vm.state.current_frame() else {
            return;
        };
        let opcode = vm
            .program_opcode(frame.program_index(), frame.ip())
            .map(opcode_name)
            .unwrap_or("out-of-bounds");
        *self
            .samples
            .entry((frame.program_index(), frame.ip(), opcode))
            .or_insert(0) += 1;
    }

    fn sample_native(&mut self, instruction: usize, trace: NativeBudgetTrace) {
        let first_instruction = instruction + 1;
        let last_instruction = instruction + trace.instructions;
        let first_sample = first_instruction.div_ceil(BUDGET_PROFILE_SAMPLE_INTERVAL)
            * BUDGET_PROFILE_SAMPLE_INTERVAL;
        if first_sample > last_instruction {
            return;
        }
        let samples = ((last_instruction - first_sample) / BUDGET_PROFILE_SAMPLE_INTERVAL) + 1;
        *self
            .samples
            .entry((trace.program, trace.ip, "NativeIntegerLoop"))
            .or_insert(0) += samples;
    }

    fn profile(&self, vm: &RegisterVm) -> BudgetProfile {
        let mut hotspots = self
            .samples
            .iter()
            .map(|((program, ip, opcode), samples)| (*samples, *program, *ip, *opcode))
            .collect::<Vec<_>>();
        hotspots.sort_by(|left, right| right.cmp(left));
        hotspots.truncate(BUDGET_PROFILE_TOP_LIMIT);

        let current_stack = vm
            .trace_frames()
            .into_iter()
            .map(|frame| frame.render())
            .collect::<Vec<_>>();
        let hot_spots = hotspots
            .into_iter()
            .map(|(samples, program, ip, opcode)| {
                BudgetTraceFrame {
                    depth: 0,
                    program,
                    program_id: vm.program_id_label(program),
                    ip,
                    opcode,
                    samples,
                }
                .render()
            })
            .collect::<Vec<_>>();

        BudgetProfile {
            current_stack,
            hot_spots,
        }
    }

    fn emit(&self, budget: usize, profile: &BudgetProfile) {
        tracing::error!(
            target: "mica_vm::budget",
            budget,
            sample_interval = BUDGET_PROFILE_SAMPLE_INTERVAL,
            frames = profile.current_stack.len(),
            current_stack = ?profile.current_stack,
            hot_spots = ?profile.hot_spots,
            "VM instruction budget exhausted"
        );
    }
}

#[derive(Clone, Debug)]
struct BudgetProfile {
    current_stack: Vec<String>,
    hot_spots: Vec<String>,
}

#[derive(Clone, Debug)]
struct BudgetTraceFrame {
    depth: usize,
    program: usize,
    program_id: Option<String>,
    ip: usize,
    opcode: &'static str,
    samples: usize,
}

impl BudgetTraceFrame {
    fn render(&self) -> String {
        let program = self.program_id.as_deref().unwrap_or("<anonymous program>");
        if self.samples == 0 {
            return format!(
                "#{depth} program[{program_index}] {program} ip={ip} opcode={opcode}",
                depth = self.depth,
                program_index = self.program,
                ip = self.ip,
                opcode = self.opcode
            );
        }

        format!(
            "samples={samples} program[{program_index}] {program} ip={ip} opcode={opcode}",
            samples = self.samples,
            program_index = self.program,
            ip = self.ip,
            opcode = self.opcode
        )
    }
}

impl<'ctx, 'kernel> VmHostContext<'ctx, 'kernel> {
    pub fn new(
        tx: &'ctx mut Transaction<'kernel>,
        authority: &'ctx mut AuthorityContext,
        resolver: &'ctx ProgramResolver,
        builtins: &'ctx BuiltinRegistry,
        ports: RuntimePorts<'ctx>,
        task_snapshot: &'ctx [Value],
        runtime_context: RuntimeContext,
    ) -> Self {
        Self {
            tx,
            authority,
            resolver,
            builtins,
            ports,
            task_snapshot,
            runtime_context,
            trace: VmHostTrace::new(),
        }
    }

    pub fn emit_trace_summary(&self, task_id: u64) {
        self.trace.emit_summary(task_id);
    }

    fn relation_metadata(&self, relation: RelationId) -> Option<RelationMetadata> {
        self.tx.relation_metadata(relation)
    }

    fn system_row_allowed(&self, metadata: Option<&RelationMetadata>, tuple: &Tuple) -> bool {
        metadata
            .and_then(|metadata| system_row_source_relation(metadata, tuple))
            .is_none_or(|source_relation| self.authority.can_read_relation(source_relation))
    }

    fn filter_authorized_system_rows(&self, relation: RelationId, rows: Vec<Tuple>) -> Vec<Tuple> {
        let metadata = self.relation_metadata(relation);
        rows.into_iter()
            .filter(|tuple| self.system_row_allowed(metadata.as_ref(), tuple))
            .collect()
    }
}

impl RelationRead for VmHostContext<'_, '_> {
    fn scan_relation(
        &self,
        relation: mica_relation_kernel::RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, mica_relation_kernel::KernelError> {
        let metrics_start = Instant::now();
        let trace_start = self.trace.start();
        let rows = self.tx.scan_relation(relation, bindings)?;
        let rows = self.filter_authorized_system_rows(relation, rows);
        let metadata = self.relation_metadata(relation);
        crate::metrics::record_relation_operation(
            RelationOperation::Scan,
            relation,
            metadata
                .as_ref()
                .and_then(|metadata| metadata.name().name()),
            bindings,
            rows.len(),
            metrics_start.elapsed(),
        );
        self.trace
            .record_relation("scan", relation, bindings, rows.len(), trace_start);
        Ok(rows)
    }

    fn visit_relation(
        &self,
        relation: mica_relation_kernel::RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, mica_relation_kernel::KernelError>,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        let metrics_start = Instant::now();
        let trace_start = self.trace.start();
        let mut rows = 0usize;
        let metadata = self.relation_metadata(relation);
        let result = self.tx.visit_relation(relation, bindings, &mut |tuple| {
            if !self.system_row_allowed(metadata.as_ref(), tuple) {
                return Ok(ScanControl::Continue);
            }
            rows += 1;
            visitor(tuple)
        });
        if result.is_ok() {
            crate::metrics::record_relation_operation(
                RelationOperation::Visit,
                relation,
                metadata
                    .as_ref()
                    .and_then(|metadata| metadata.name().name()),
                bindings,
                rows,
                metrics_start.elapsed(),
            );
        }
        self.trace
            .record_relation("visit", relation, bindings, rows, trace_start);
        result
    }
}

impl DispatchRead for VmHostContext<'_, '_> {
    fn cached_applicable_method_calls(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        roles: &[(Value, Value)],
    ) -> Result<Option<Vec<ApplicableMethodCall>>, mica_relation_kernel::KernelError> {
        let start = self.trace.start();
        let calls =
            DispatchRead::cached_applicable_method_calls(&*self.tx, relations, selector, roles)?;
        self.trace.record_dispatch(
            "applicable_methods",
            selector,
            roles.len(),
            calls.as_ref().map_or(0, Vec::len),
            start,
        );
        Ok(calls)
    }

    fn cached_method_program(
        &self,
        relation: mica_relation_kernel::RelationId,
        method: &Value,
    ) -> Result<Option<Option<Value>>, mica_relation_kernel::KernelError> {
        let start = self.trace.start();
        let program = DispatchRead::cached_method_program(&*self.tx, relation, method)?;
        self.trace
            .record_method_program(relation, method, program.is_some(), start);
        Ok(program)
    }

    fn cached_applicable_positional_methods(
        &self,
        relations: DispatchRelations,
        selector: &Value,
        args: &[Value],
    ) -> Result<Option<Arc<[Value]>>, mica_relation_kernel::KernelError> {
        let start = self.trace.start();
        let methods = DispatchRead::cached_applicable_positional_methods(
            &*self.tx, relations, selector, args,
        )?;
        self.trace.record_dispatch(
            "applicable_positional_methods",
            selector,
            args.len(),
            methods.as_ref().map_or(0, |methods| methods.len()),
            start,
        );
        Ok(methods)
    }
}

impl RelationWorkspace for VmHostContext<'_, '_> {
    fn assert_tuple(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        tuple: Tuple,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.tx.assert_tuple(relation, tuple)
    }

    fn retract_tuple(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        tuple: Tuple,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.tx.retract_tuple(relation, tuple)
    }

    fn replace_functional_tuple(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        tuple: Tuple,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.tx.replace_functional_tuple(relation, tuple)
    }

    fn retract_matching(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        bindings: &[Option<Value>],
    ) -> Result<(), mica_relation_kernel::KernelError> {
        let tuples = self.scan_relation(relation, bindings)?;
        for tuple in tuples {
            self.retract_tuple(relation, tuple)?;
        }
        Ok(())
    }
}

struct VmHostTrace {
    enabled: bool,
    detail_threshold: Duration,
    stats: RefCell<VmHostTraceStats>,
}

#[derive(Default)]
struct VmHostTraceStats {
    scan_count: usize,
    scan_rows: usize,
    scan_time: Duration,
    visit_count: usize,
    visit_rows: usize,
    visit_time: Duration,
    dispatch_count: usize,
    dispatch_calls: usize,
    dispatch_time: Duration,
    method_program_count: usize,
    method_program_hits: usize,
    method_program_time: Duration,
}

impl VmHostTrace {
    fn new() -> Self {
        Self {
            enabled: vm_host_trace_enabled(),
            detail_threshold: vm_host_trace_detail_threshold(),
            stats: RefCell::new(VmHostTraceStats::default()),
        }
    }

    fn start(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    fn record_relation(
        &self,
        op: &'static str,
        relation: RelationId,
        bindings: &[Option<Value>],
        rows: usize,
        start: Option<Instant>,
    ) {
        let Some(start) = start else {
            return;
        };
        let elapsed = start.elapsed();
        {
            let mut stats = self.stats.borrow_mut();
            if op == "scan" {
                stats.scan_count += 1;
                stats.scan_rows += rows;
                stats.scan_time += elapsed;
            } else {
                stats.visit_count += 1;
                stats.visit_rows += rows;
                stats.visit_time += elapsed;
            }
        }
        if elapsed < self.detail_threshold {
            return;
        }
        tracing::trace!(
            target: "mica_vm::host",
            op,
            relation = ?relation,
            bound = bindings.iter().filter(|binding| binding.is_some()).count(),
            rows,
            elapsed_us = elapsed.as_micros(),
            "VM host relation operation"
        );
    }

    fn record_dispatch(
        &self,
        op: &'static str,
        selector: &Value,
        roles: usize,
        calls: usize,
        start: Option<Instant>,
    ) {
        let Some(start) = start else {
            return;
        };
        let elapsed = start.elapsed();
        {
            let mut stats = self.stats.borrow_mut();
            stats.dispatch_count += 1;
            stats.dispatch_calls += calls;
            stats.dispatch_time += elapsed;
        }
        if elapsed < self.detail_threshold {
            return;
        }
        tracing::trace!(
            target: "mica_vm::host",
            op,
            selector = %selector,
            roles,
            calls,
            elapsed_us = elapsed.as_micros(),
            "VM host dispatch operation"
        );
    }

    fn record_method_program(
        &self,
        relation: RelationId,
        method: &Value,
        found: bool,
        start: Option<Instant>,
    ) {
        let Some(start) = start else {
            return;
        };
        let elapsed = start.elapsed();
        {
            let mut stats = self.stats.borrow_mut();
            stats.method_program_count += 1;
            stats.method_program_hits += usize::from(found);
            stats.method_program_time += elapsed;
        }
        if elapsed < self.detail_threshold {
            return;
        }
        tracing::trace!(
            target: "mica_vm::host",
            op = "method_program",
            relation = ?relation,
            method = %method,
            found,
            elapsed_us = elapsed.as_micros(),
            "VM host method program lookup"
        );
    }

    fn emit_summary(&self, task_id: u64) {
        if !self.enabled {
            return;
        }
        let stats = self.stats.borrow();
        if stats.scan_count == 0
            && stats.visit_count == 0
            && stats.dispatch_count == 0
            && stats.method_program_count == 0
        {
            return;
        }
        tracing::trace!(
            target: "mica_vm::host",
            task_id,
            scans = stats.scan_count,
            scan_rows = stats.scan_rows,
            scan_time_us = stats.scan_time.as_micros(),
            visits = stats.visit_count,
            visit_rows = stats.visit_rows,
            visit_time_us = stats.visit_time.as_micros(),
            dispatches = stats.dispatch_count,
            dispatch_calls = stats.dispatch_calls,
            dispatch_time_us = stats.dispatch_time.as_micros(),
            method_programs = stats.method_program_count,
            method_program_hits = stats.method_program_hits,
            method_program_time_us = stats.method_program_time.as_micros(),
            "VM host trace summary"
        );
    }
}

fn vm_host_trace_enabled() -> bool {
    tracing::enabled!(target: "mica_vm::host", tracing::Level::TRACE)
}

fn vm_host_trace_detail_threshold() -> Duration {
    static THRESHOLD: OnceLock<Duration> = OnceLock::new();
    *THRESHOLD.get_or_init(|| {
        std::env::var("MICA_VM_HOST_TRACE_DETAIL_US")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_micros)
            .unwrap_or_else(|| Duration::from_millis(10))
    })
}

impl VmHost for VmHostContext<'_, '_> {
    fn authority(&self) -> &AuthorityContext {
        self.authority
    }

    fn authority_mut(&mut self) -> &mut AuthorityContext {
        self.authority
    }

    fn emit(&mut self, target: Identity, value: Value) -> Result<(), RuntimeError> {
        if !self.authority.can_effect() {
            return Err(RuntimeError::PermissionDenied {
                operation: "effect",
                target: Value::identity(target),
            });
        }
        self.ports
            .pending_effects
            .push(Emission::new(target, value));
        Ok(())
    }

    fn validate_mailbox_receiver(&mut self, receiver: &Value) -> Result<(), RuntimeError> {
        let Some(mailbox_runtime) = self.ports.mailbox_runtime.as_ref() else {
            return Err(RuntimeError::InvalidBuiltinCall {
                name: Symbol::intern("mailbox_recv"),
                message: "mailbox runtime is not available".to_owned(),
            });
        };
        mailbox_runtime.validate_mailbox_receiver(receiver)
    }

    fn resolve_program(
        &mut self,
        program_bytes_relation: mica_relation_kernel::RelationId,
        program_id: &Value,
    ) -> Result<Arc<Program>, RuntimeError> {
        self.resolver
            .resolve(self, program_bytes_relation, program_id)
    }

    fn call_builtin(&mut self, name: Symbol, args: &[Value]) -> Result<Value, RuntimeError> {
        let builtin = self
            .builtins
            .get(name)
            .ok_or(RuntimeError::UnknownBuiltin { name })?;
        let mut context = crate::BuiltinContext::new(
            self.tx.kernel(),
            self.tx,
            self.authority,
            RuntimePorts {
                pending_effects: self.ports.pending_effects,
                pending_mailbox_sends: self.ports.pending_mailbox_sends,
                pending_subscriptions: self.ports.pending_subscriptions,
                mailbox_runtime: self.ports.mailbox_runtime,
            },
            self.task_snapshot,
            self.runtime_context,
        );
        builtin.call(&mut context, args)
    }
}

pub struct ProjectedVmHostContext<'ctx, W> {
    workspace: &'ctx mut W,
    authority: &'ctx mut AuthorityContext,
    resolver: &'ctx ProgramResolver,
    builtins: Option<&'ctx ClientBuiltinRegistry>,
    pending_effects: &'ctx mut Vec<Emission>,
    runtime_context: RuntimeContext,
}

impl<'ctx, W> ProjectedVmHostContext<'ctx, W> {
    pub fn new(
        workspace: &'ctx mut W,
        authority: &'ctx mut AuthorityContext,
        resolver: &'ctx ProgramResolver,
        pending_effects: &'ctx mut Vec<Emission>,
    ) -> Self {
        Self {
            workspace,
            authority,
            resolver,
            builtins: None,
            pending_effects,
            runtime_context: RuntimeContext::default(),
        }
    }

    pub fn with_builtins(mut self, builtins: &'ctx ClientBuiltinRegistry) -> Self {
        self.builtins = Some(builtins);
        self
    }

    pub fn with_runtime_context(mut self, runtime_context: RuntimeContext) -> Self {
        self.runtime_context = runtime_context;
        self
    }
}

impl<W: RelationWorkspace> RelationRead for ProjectedVmHostContext<'_, W> {
    fn scan_relation(
        &self,
        relation: mica_relation_kernel::RelationId,
        bindings: &[Option<Value>],
    ) -> Result<Vec<Tuple>, mica_relation_kernel::KernelError> {
        self.workspace.scan_relation(relation, bindings)
    }

    fn visit_relation(
        &self,
        relation: mica_relation_kernel::RelationId,
        bindings: &[Option<Value>],
        visitor: &mut dyn FnMut(&Tuple) -> Result<ScanControl, mica_relation_kernel::KernelError>,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.workspace.visit_relation(relation, bindings, visitor)
    }
}

impl<W: RelationWorkspace> RelationWorkspace for ProjectedVmHostContext<'_, W> {
    fn assert_tuple(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        tuple: Tuple,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.workspace.assert_tuple(relation, tuple)
    }

    fn retract_tuple(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        tuple: Tuple,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.workspace.retract_tuple(relation, tuple)
    }

    fn replace_functional_tuple(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        tuple: Tuple,
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.workspace.replace_functional_tuple(relation, tuple)
    }

    fn retract_matching(
        &mut self,
        relation: mica_relation_kernel::RelationId,
        bindings: &[Option<Value>],
    ) -> Result<(), mica_relation_kernel::KernelError> {
        self.workspace.retract_matching(relation, bindings)
    }
}

impl<W: RelationWorkspace> DispatchRead for ProjectedVmHostContext<'_, W> {}

impl<W: RelationWorkspace> VmHost for ProjectedVmHostContext<'_, W> {
    fn authority(&self) -> &AuthorityContext {
        self.authority
    }

    fn authority_mut(&mut self) -> &mut AuthorityContext {
        self.authority
    }

    fn emit(&mut self, target: Identity, value: Value) -> Result<(), RuntimeError> {
        if !self.authority.can_effect() {
            return Err(RuntimeError::PermissionDenied {
                operation: "effect",
                target: Value::identity(target),
            });
        }
        self.pending_effects.push(Emission::new(target, value));
        Ok(())
    }

    fn validate_mailbox_receiver(&mut self, receiver: &Value) -> Result<(), RuntimeError> {
        Err(RuntimeError::InvalidMailboxCapability {
            operation: "recv",
            capability: receiver.clone(),
        })
    }

    fn resolve_program(
        &mut self,
        program_bytes_relation: mica_relation_kernel::RelationId,
        program_id: &Value,
    ) -> Result<Arc<Program>, RuntimeError> {
        self.resolver
            .resolve(self, program_bytes_relation, program_id)
    }

    fn call_builtin(&mut self, name: Symbol, args: &[Value]) -> Result<Value, RuntimeError> {
        let builtin = self
            .builtins
            .and_then(|builtins| builtins.get(name))
            .ok_or(RuntimeError::UnknownBuiltin { name })?;
        let workspace: &mut dyn RelationWorkspace = self.workspace;
        let mut context = ClientBuiltinContext::new(
            workspace,
            self.authority,
            self.pending_effects,
            self.runtime_context,
        );
        builtin.call(&mut context, args)
    }
}

fn opcode_name(opcode: &Opcode) -> &'static str {
    match opcode {
        Opcode::Load { .. } => "Load",
        Opcode::Move { .. } => "Move",
        Opcode::CheckKind { .. } => "CheckKind",
        Opcode::CheckType { .. } => "CheckType",
        Opcode::Unary { .. } => "Unary",
        Opcode::Binary { .. } => "Binary",
        Opcode::BuildList { .. } => "BuildList",
        Opcode::BuildRelation { .. } => "BuildRelation",
        Opcode::BuildMap { .. } => "BuildMap",
        Opcode::BuildMapDynamic { .. } => "BuildMapDynamic",
        Opcode::BuildRange { .. } => "BuildRange",
        Opcode::Index { .. } => "Index",
        Opcode::SetIndex { .. } => "SetIndex",
        Opcode::ErrorField { .. } => "ErrorField",
        Opcode::One { .. } => "One",
        Opcode::CollectionLen { .. } => "CollectionLen",
        Opcode::CollectionKeyAt { .. } => "CollectionKeyAt",
        Opcode::CollectionValueAt { .. } => "CollectionValueAt",
        Opcode::RelationPattern { .. } => "RelationPattern",
        Opcode::RelationCell { .. } => "RelationCell",
        Opcode::RelationCellAt { .. } => "RelationCellAt",
        Opcode::ScanExists { .. } => "ScanExists",
        Opcode::ScanBindings { .. } => "ScanBindings",
        Opcode::ScanValue { .. } => "ScanValue",
        Opcode::Assert { .. } => "Assert",
        Opcode::Retract { .. } => "Retract",
        Opcode::RetractWhere { .. } => "RetractWhere",
        Opcode::ScanDynamic { .. } => "ScanDynamic",
        Opcode::AssertDynamic { .. } => "AssertDynamic",
        Opcode::RetractDynamic { .. } => "RetractDynamic",
        Opcode::ReplaceFunctional { .. } => "ReplaceFunctional",
        Opcode::Branch { .. } => "Branch",
        Opcode::Jump { .. } => "Jump",
        Opcode::EnterTry { .. } => "EnterTry",
        Opcode::ExitTry => "ExitTry",
        Opcode::EndFinally => "EndFinally",
        Opcode::Emit { .. } => "Emit",
        Opcode::LoadFunction { .. } => "LoadFunction",
        Opcode::CallValue { .. } => "CallValue",
        Opcode::CallValueDynamic { .. } => "CallValueDynamic",
        Opcode::Call { .. } => "Call",
        Opcode::BuiltinCall { .. } => "BuiltinCall",
        Opcode::BuiltinCallDynamic { .. } => "BuiltinCallDynamic",
        Opcode::Dispatch { .. } => "Dispatch",
        Opcode::DynamicDispatch { .. } => "DynamicDispatch",
        Opcode::PositionalDispatch { .. } => "PositionalDispatch",
        Opcode::PositionalDispatchDynamic { .. } => "PositionalDispatchDynamic",
        Opcode::SpawnDispatch { .. } => "SpawnDispatch",
        Opcode::SpawnDispatchDynamic { .. } => "SpawnDispatchDynamic",
        Opcode::SpawnPositionalDispatch { .. } => "SpawnPositionalDispatch",
        Opcode::SpawnPositionalDispatchDynamic { .. } => "SpawnPositionalDispatchDynamic",
        Opcode::Commit => "Commit",
        Opcode::Suspend { .. } => "Suspend",
        Opcode::SuspendValue { .. } => "SuspendValue",
        Opcode::CommitValue { .. } => "CommitValue",
        Opcode::Read { .. } => "Read",
        Opcode::MailboxRecv { .. } => "MailboxRecv",
        Opcode::ExternalRequest { .. } => "ExternalRequest",
        Opcode::RollbackRetry => "RollbackRetry",
        Opcode::Return { .. } => "Return",
        Opcode::Abort { .. } => "Abort",
        Opcode::Raise { .. } => "Raise",
    }
}

#[derive(Clone, Debug)]
pub struct RegisterVm {
    state: VmState,
    type_check_cache: BTreeMap<usize, (Value, BTreeSet<TypeContract>)>,
    #[cfg(feature = "cranelift")]
    native_execution: bool,
    #[cfg(feature = "cranelift")]
    native_side_exits: Vec<(usize, usize)>,
}

#[derive(Clone, Copy)]
struct NativeBudgetTrace {
    program: usize,
    ip: usize,
    instructions: usize,
}

struct VmStep {
    response: VmHostResponse,
    instructions: usize,
    native_trace: Option<NativeBudgetTrace>,
}

impl VmStep {
    fn single(response: VmHostResponse) -> Self {
        Self {
            response,
            instructions: 1,
            native_trace: None,
        }
    }
}

impl RegisterVm {
    pub fn new(program: Arc<Program>) -> Self {
        let register_count = program.register_count();
        Self {
            state: VmState {
                programs: vec![program],
                functions: Vec::new(),
                resolved_programs: Vec::new(),
                frames: vec![Frame::root(0, register_count)],
                pending_resume: None,
            },
            type_check_cache: BTreeMap::new(),
            #[cfg(feature = "cranelift")]
            native_execution: true,
            #[cfg(feature = "cranelift")]
            native_side_exits: Vec::new(),
        }
    }

    pub fn new_interpreted(program: Arc<Program>) -> Self {
        let vm = Self::new(program);
        #[cfg(feature = "cranelift")]
        let vm = {
            let mut vm = vm;
            vm.native_execution = false;
            vm
        };
        vm
    }

    pub fn disable_native_execution(&mut self) {
        #[cfg(feature = "cranelift")]
        {
            self.native_execution = false;
        }
    }

    #[cfg(all(feature = "cranelift", test))]
    pub(crate) fn native_side_exit_count(&self) -> usize {
        self.native_side_exits.len()
    }

    pub fn from_state(state: VmState) -> Self {
        Self {
            state,
            type_check_cache: BTreeMap::new(),
            #[cfg(feature = "cranelift")]
            native_execution: true,
            #[cfg(feature = "cranelift")]
            native_side_exits: Vec::new(),
        }
    }

    pub fn snapshot_state(&self) -> VmState {
        self.state.clone()
    }

    pub fn restore_state(&mut self, state: &VmState) {
        self.state = state.clone();
        self.type_check_cache.clear();
        #[cfg(feature = "cranelift")]
        self.native_side_exits.clear();
    }

    pub fn resume_with(&mut self, value: Value) -> Result<(), RuntimeError> {
        let Some(register) = self.state.pending_resume else {
            return Ok(());
        };
        self.write_register(register, value)?;
        self.state.pending_resume = None;
        Ok(())
    }

    pub fn frame_count(&self) -> usize {
        self.state.frames.len()
    }

    pub fn register(&self, register: Register) -> Option<&Value> {
        self.current_frame()
            .ok()
            .and_then(|frame| frame.registers.get(register.0 as usize))
    }

    pub fn set_register(&mut self, register: Register, value: Value) -> Result<(), RuntimeError> {
        self.write_register(register, value)
    }

    fn program_opcode(&self, program: usize, ip: usize) -> Option<&Opcode> {
        self.state
            .programs
            .get(program)
            .and_then(|program| program.opcodes().get(ip))
    }

    fn program_id_label(&self, program: usize) -> Option<String> {
        self.state
            .resolved_programs
            .iter()
            .find(|(_, index)| *index == program)
            .map(|(program_id, _)| format!("{program_id:?}"))
    }

    fn trace_frames(&self) -> Vec<BudgetTraceFrame> {
        self.state
            .frames
            .iter()
            .enumerate()
            .map(|(depth, frame)| {
                let opcode = self
                    .program_opcode(frame.program_index(), frame.ip())
                    .map(opcode_name)
                    .unwrap_or("out-of-bounds");
                BudgetTraceFrame {
                    depth,
                    program: frame.program_index(),
                    program_id: self.program_id_label(frame.program_index()),
                    ip: frame.ip(),
                    opcode,
                    samples: 0,
                }
            })
            .collect()
    }

    pub fn run_until_host_response<H: VmHost>(
        &mut self,
        host: &mut H,
        instruction_budget: usize,
        max_call_depth: usize,
    ) -> Result<VmHostResponse, RuntimeError> {
        let mut profiler = BudgetProfiler::new();
        let mut instruction = 0;
        while instruction < instruction_budget {
            profiler.sample(instruction, self);
            let remaining_budget = instruction_budget - instruction;
            let step = self.step(host, max_call_depth, remaining_budget)?;
            debug_assert!(step.instructions > 0);
            debug_assert!(step.instructions <= remaining_budget);
            if let Some(trace) = step.native_trace {
                profiler.sample_native(instruction, trace);
            }
            instruction += step.instructions;
            if step.response != VmHostResponse::Continue {
                return Ok(step.response);
            }
        }
        let profile = profiler.profile(self);
        profiler.emit(instruction_budget, &profile);
        Err(RuntimeError::InstructionBudgetExceeded {
            budget: instruction_budget,
            current_stack: profile.current_stack,
            hot_spots: profile.hot_spots,
        })
    }

    fn step<H: VmHost>(
        &mut self,
        host: &mut H,
        max_call_depth: usize,
        remaining_budget: usize,
    ) -> Result<VmStep, RuntimeError> {
        #[cfg(not(feature = "cranelift"))]
        let _ = remaining_budget;
        let (opcode, program, ip) = {
            let frame = self.current_frame_unchecked();
            let ip = frame.ip;
            let frame_program = &self.state.programs[frame.program];
            let program = Arc::as_ptr(frame_program);
            let opcode = frame_program
                .opcodes()
                .get(ip)
                .ok_or(RuntimeError::ProgramCounterOutOfBounds { ip })?;
            (opcode as *const Opcode, program, ip)
        };
        debug_assert_eq!(self.current_frame_unchecked().ip, ip);
        // SAFETY: both pointers come from an `Arc<Program>` owned by the VM state's program table.
        // The program allocation is stable while that table entry is live, and each arm copies or
        // resolves any table values it needs before operations that can remove the current frame.
        let program = unsafe { &*program };
        let opcode = unsafe { &*opcode };

        match opcode {
            Opcode::Load { dst, value } => {
                self.write_register_unchecked(*dst, program.constant(*value).clone());
                self.advance_ip_unchecked();
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::CheckKind {
                value,
                expected,
                site,
                subject,
            } => {
                let value = self.read_register_unchecked(*value);
                let actual = value.kind();
                if actual != *expected {
                    let error = kind_check_error(*site, *subject, *expected, actual, value.clone());
                    return self.begin_raise(error).map(VmStep::single);
                }
                self.advance_ip_unchecked();
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::CheckType {
                value,
                expected,
                site,
                subject,
            } => {
                let value = self.read_register_unchecked(*value).clone();
                if !value_matches_type(&value, expected, &mut self.type_check_cache) {
                    let error = type_check_error(*site, *subject, expected, value);
                    return self.begin_raise(error).map(VmStep::single);
                }
                self.advance_ip_unchecked();
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::Binary {
                dst,
                op,
                left,
                right,
            } => {
                let value = match eval_binary(
                    *op,
                    self.read_register_unchecked(*left),
                    self.read_register_unchecked(*right),
                ) {
                    Ok(value) => value,
                    Err(error) => return self.begin_raise(error).map(VmStep::single),
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::Branch {
                condition,
                if_true,
                if_false,
            } => {
                let branch_is_true = truthy(self.read_register_unchecked(*condition));
                let target = if branch_is_true {
                    if_true.0 as usize
                } else {
                    if_false.0 as usize
                };
                #[cfg(feature = "cranelift")]
                if branch_is_true && self.native_execution {
                    if target < ip
                        && let Some(step) =
                            self.execute_native_integer_loop(program, ip, remaining_budget)
                    {
                        return Ok(step);
                    }
                    if target < ip
                        && let Some(step) =
                            self.execute_native_float_loop(program, ip, remaining_budget)
                    {
                        return Ok(step);
                    }
                    if let Some(step) =
                        self.execute_native_natural_integer_loop(program, ip, remaining_budget)
                    {
                        return Ok(step);
                    }
                }
                self.current_frame_mut_unchecked().ip = target;
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::Call {
                dst,
                program: callee,
                args,
            } => {
                if self.state.frames.len() >= max_call_depth {
                    return Err(RuntimeError::MaxCallDepthExceeded {
                        max_depth: max_call_depth,
                    });
                }
                let args = self.resolve_operands(program, program.operands(*args));
                let callee = program.program(*callee);
                let callee_id = self.intern_program(Arc::clone(callee));
                let register_count = callee.register_count();
                self.advance_ip_unchecked();
                self.state
                    .frames
                    .push(Frame::new(callee_id, register_count, Some(*dst), args)?);
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::BuiltinCall {
                dst,
                name,
                result_kind,
                args,
            } => {
                let args = self.resolve_operands(program, program.operands(*args));
                let value = match host.call_builtin(*name, &args) {
                    Ok(value) => value,
                    Err(RuntimeError::Raised(error)) => {
                        return self.begin_raise(error).map(VmStep::single);
                    }
                    Err(error) => return Err(error),
                };
                let value = validate_builtin_result(*name, *result_kind, value)?;
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmStep::single(VmHostResponse::Continue))
            }
            Opcode::Return { value } => {
                let value = self.resolve_operand_ref(program, *value);
                self.return_from_frame(value).map(VmStep::single)
            }
            _ => self
                .step_extended(host, max_call_depth, program, opcode)
                .map(VmStep::single),
        }
    }

    #[cfg(feature = "cranelift")]
    fn execute_native_integer_loop(
        &mut self,
        program: &Program,
        branch_ip: usize,
        remaining_budget: usize,
    ) -> Option<VmStep> {
        let max_iterations = remaining_budget.checked_sub(1)? / 3;
        if max_iterations == 0 {
            return None;
        }
        let site = program.integer_loop_site(branch_ip)?;
        let current = Value::int(self.read_register_unchecked(site.current).as_int()?).ok()?;
        let step = Value::int(self.read_register_unchecked(site.step).as_int()?).ok()?;
        let limit = Value::int(self.read_register_unchecked(site.limit).as_int()?).ok()?;
        let compile = native_integer_loop_is_profitable(
            current.as_int()?,
            step.as_int()?,
            limit.as_int()?,
            max_iterations,
        );
        let compiled = program.compiled_integer_loop(compile)?;
        let outcome = compiled.run(&current, &step, &limit, u64::try_from(max_iterations).ok()?);
        let (current, condition, iterations, next_ip) = match outcome {
            IntegerLoopOutcome::Complete {
                current,
                condition,
                iterations,
            } => (current, condition, iterations, site.exit_ip),
            IntegerLoopOutcome::BudgetExhausted {
                current,
                condition,
                iterations,
            } => (current, condition, iterations, site.entry_ip),
            IntegerLoopOutcome::SideExit => return None,
        };
        let iterations = usize::try_from(iterations).ok()?;
        let native_instructions = iterations.checked_mul(3)?;
        let instructions = native_instructions.checked_add(1)?;
        if instructions > remaining_budget {
            return None;
        }
        let program_index = self.current_frame_unchecked().program;
        self.write_register_unchecked(site.current, current);
        self.write_register_unchecked(site.condition, condition);
        self.current_frame_mut_unchecked().ip = next_ip;
        Some(VmStep {
            response: VmHostResponse::Continue,
            instructions,
            native_trace: Some(NativeBudgetTrace {
                program: program_index,
                ip: site.entry_ip,
                instructions: native_instructions,
            }),
        })
    }

    #[cfg(feature = "cranelift")]
    fn execute_native_float_loop(
        &mut self,
        program: &Program,
        branch_ip: usize,
        remaining_budget: usize,
    ) -> Option<VmStep> {
        let max_iterations = remaining_budget.checked_sub(1)? / 3;
        if max_iterations == 0 {
            return None;
        }
        let site = program.float_loop_site(branch_ip)?;
        let current = Value::float(self.read_register_unchecked(site.current).as_float()?).ok()?;
        let step = Value::float(self.read_register_unchecked(site.step).as_float()?).ok()?;
        let limit = Value::float(self.read_register_unchecked(site.limit).as_float()?).ok()?;
        let compiled = if let Some(compiled) = program.compiled_float_loop(branch_ip, false) {
            compiled
        } else {
            let compile = native_float_loop_is_profitable(
                current.as_float()?,
                step.as_float()?,
                limit.as_float()?,
                site.plan,
                max_iterations,
            );
            program.compiled_float_loop(branch_ip, compile)?
        };
        let outcome = compiled.run(&current, &step, &limit, u64::try_from(max_iterations).ok()?);
        let (current, condition, iterations, next_ip) = match outcome {
            FloatLoopOutcome::Complete {
                current,
                condition,
                iterations,
            } => (current, condition, iterations, site.exit_ip),
            FloatLoopOutcome::BudgetExhausted {
                current,
                condition,
                iterations,
            } => (current, condition, iterations, site.entry_ip),
            FloatLoopOutcome::SideExit => return None,
        };
        let iterations = usize::try_from(iterations).ok()?;
        let native_instructions = iterations.checked_mul(3)?;
        let instructions = native_instructions.checked_add(1)?;
        if instructions > remaining_budget {
            return None;
        }
        let program_index = self.current_frame_unchecked().program;
        self.write_register_unchecked(site.current, current);
        self.write_register_unchecked(site.condition, condition);
        self.current_frame_mut_unchecked().ip = next_ip;
        Some(VmStep {
            response: VmHostResponse::Continue,
            instructions,
            native_trace: Some(NativeBudgetTrace {
                program: program_index,
                ip: site.entry_ip,
                instructions: native_instructions,
            }),
        })
    }

    #[cfg(feature = "cranelift")]
    fn execute_native_natural_integer_loop(
        &mut self,
        program: &Program,
        branch_ip: usize,
        remaining_budget: usize,
    ) -> Option<VmStep> {
        let program_index = self.current_frame_unchecked().program;
        if self.native_side_exits.contains(&(program_index, branch_ip)) {
            return None;
        }
        let site = program.natural_integer_loop_site(branch_ip)?;
        let native_budget = remaining_budget.checked_sub(1)?;
        let max_iterations = native_budget / site.region_instruction_count;
        if max_iterations == 0 {
            return None;
        }
        let current = self.read_register_unchecked(site.current).as_int()?;
        let limit = self.read_register_unchecked(site.limit).as_int()?;
        let compile = natural_integer_loop_is_profitable(
            current,
            site.delta,
            limit,
            site.comparison,
            max_iterations,
            if site.collection_view_registers.is_empty() {
                MIN_NATIVE_INTEGER_LOOP_ITERATIONS
            } else {
                MIN_NATIVE_COLLECTION_LOOP_ITERATIONS
            },
        );
        let compiled = program.compiled_natural_integer_loop(branch_ip, compile)?;
        let mut collection_views =
            [NaturalLoopCollectionView::default(); MAX_NATURAL_LOOP_COLLECTION_VIEWS];
        for (view, register) in site.collection_view_registers.iter().copied().enumerate() {
            let Some(collection_view) =
                natural_loop_collection_view(self.read_register_unchecked(register))
            else {
                self.native_side_exits.push((program_index, branch_ip));
                return None;
            };
            collection_views[view] = collection_view;
        }
        let mut scratch = [0_u64; MAX_NATURAL_LOOP_SLOTS];
        for (slot, register) in site.registers.iter().enumerate() {
            scratch[slot] = borrowed_value_bits(self.read_register_unchecked(*register));
        }
        let outcome = compiled.run(
            &mut scratch[..site.registers.len()],
            &collection_views[..site.collection_view_registers.len()],
            u64::try_from(native_budget).ok()?,
        );
        let (native_instructions, next_ip, modified_slots) = match outcome {
            NaturalLoopOutcome::Complete {
                instructions,
                modified_slots,
            } => (instructions, site.exit_ip, modified_slots),
            NaturalLoopOutcome::BudgetExhausted {
                instructions,
                resume,
                modified_slots,
            } => (
                instructions,
                site.header_ip.checked_add(usize::from(resume))?,
                modified_slots,
            ),
            NaturalLoopOutcome::SideExit => {
                self.native_side_exits.push((program_index, branch_ip));
                return None;
            }
        };
        let native_instructions = usize::try_from(native_instructions).ok()?;
        let instructions = native_instructions.checked_add(1)?;
        if instructions > remaining_budget {
            return None;
        }
        for (slot, register) in site.registers.iter().copied().enumerate() {
            if modified_slots & (1_u32 << slot) == 0 {
                continue;
            }
            let bits = scratch[slot];
            let value = if value_is_immediate(bits) {
                // SAFETY: generated code produces valid immediate words.
                unsafe { from_owned_value_bits(bits) }
            } else {
                // SAFETY: non-immediate generated outputs are borrowed words
                // loaded from an immutable collection view. The collection's
                // VM register owns their storage throughout this call.
                let owned_bits = unsafe { clone_value_bits(bits) };
                // SAFETY: clone_value_bits transferred one live strong
                // reference into owned_bits immediately above.
                unsafe { from_owned_value_bits(owned_bits) }
            };
            self.write_register_unchecked(register, value);
        }
        self.current_frame_mut_unchecked().ip = next_ip;
        Some(VmStep {
            response: VmHostResponse::Continue,
            instructions,
            native_trace: Some(NativeBudgetTrace {
                program: program_index,
                ip: site.header_ip,
                instructions: native_instructions,
            }),
        })
    }

    fn step_extended<H: VmHost>(
        &mut self,
        host: &mut H,
        max_call_depth: usize,
        program: &Program,
        opcode: &Opcode,
    ) -> Result<VmHostResponse, RuntimeError> {
        match opcode {
            Opcode::Load { .. }
            | Opcode::CheckKind { .. }
            | Opcode::CheckType { .. }
            | Opcode::Binary { .. }
            | Opcode::Branch { .. }
            | Opcode::Call { .. }
            | Opcode::BuiltinCall { .. }
            | Opcode::Return { .. } => unreachable!("core opcode handled by RegisterVm::step"),
            Opcode::Move { dst, src } => {
                let value = self.read_register_unchecked(*src).clone();
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::Unary { dst, op, src } => {
                let value = self.read_register_unchecked(*src);
                let value = match eval_unary(*op, value) {
                    Ok(value) => value,
                    Err(error) => return self.begin_raise(error),
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::BuildList { dst, items } => {
                let value = self.build_list(program, program.list_items(*items));
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::BuildRelation {
                dst,
                heading,
                cells,
                row_count,
            } => {
                let heading = program
                    .operands(*heading)
                    .iter()
                    .map(|operand| {
                        let OperandRef::Constant(id) = operand else {
                            unreachable!("validated relation headings contain constants");
                        };
                        program
                            .constant(*id)
                            .as_symbol()
                            .expect("validated relation headings contain symbols")
                    })
                    .collect::<Vec<_>>();
                let cells = self.resolve_operands(program, program.operands(*cells));
                let arity = heading.len();
                let rows = if arity == 0 {
                    (0..usize::from(*row_count))
                        .map(|_| Tuple::new([]))
                        .collect::<Vec<_>>()
                } else {
                    cells
                        .chunks_exact(arity)
                        .map(|row| Tuple::new(row.iter().cloned()))
                        .collect()
                };
                let value = if *row_count <= 1 {
                    Value::small_relation(heading, rows.into_iter().next())
                } else {
                    Value::relation(heading, rows)
                }
                .map_err(|error| {
                    RuntimeError::ProgramArtifact(format!("invalid relation literal: {error}"))
                })?;
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::BuildMap { dst, entries } => {
                let entries = program
                    .map_entries(*entries)
                    .iter()
                    .map(|(key, value)| {
                        (
                            self.resolve_operand_ref(program, *key),
                            self.resolve_operand_ref(program, *value),
                        )
                    })
                    .collect::<Vec<_>>();
                self.write_register_unchecked(*dst, Value::map(entries));
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::BuildMapDynamic { dst, items } => {
                let value = self.resolve_map_items(program, program.map_items(*items))?;
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::BuildRange { dst, start, end } => {
                let start = self.resolve_operand_ref(program, *start);
                let end = end.map(|end| self.resolve_operand_ref(program, end));
                self.write_register_unchecked(*dst, Value::range(start, end));
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::Index {
                dst,
                collection,
                index,
            } => {
                let value = match index_value(
                    self.read_register_unchecked(*collection),
                    &self.resolve_operand_ref(program, *index),
                ) {
                    Ok(value) => value,
                    Err(error) => return self.begin_raise(error),
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::SetIndex {
                dst,
                collection,
                index,
                value,
            } => {
                let value = match set_index_value(
                    self.read_register_unchecked(*collection),
                    &self.resolve_operand_ref(program, *index),
                    self.resolve_operand_ref(program, *value),
                ) {
                    Ok(value) => value,
                    Err(error) => return self.begin_raise(error),
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ErrorField { dst, error, field } => {
                let value = error_field_value(self.read_register_unchecked(*error), *field);
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::One { dst, src } => {
                let value = match one_value(self.read_register_unchecked(*src)) {
                    Ok(value) => value,
                    Err(error) => return self.begin_raise(error),
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::CollectionLen { dst, collection } => {
                let value = collection_len(self.read_register_unchecked(*collection));
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::CollectionKeyAt {
                dst,
                collection,
                index,
            } => {
                let value = collection_key_at(
                    self.read_register_unchecked(*collection),
                    self.read_register_unchecked(*index),
                );
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::CollectionValueAt {
                dst,
                collection,
                index,
            } => {
                let value = collection_value_at(
                    self.read_register_unchecked(*collection),
                    self.read_register_unchecked(*index),
                );
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::RelationPattern {
                dst,
                relation,
                heading,
                row_count,
                equalities,
            } => {
                let matched =
                    self.read_register_unchecked(*relation)
                        .with_relation(|value| {
                            value.len() == usize::from(*row_count)
                                && value.heading().len() == program.operands(*heading).len()
                                && value.heading().iter().zip(program.operands(*heading)).all(
                                    |(actual, expected)| {
                                        *actual
                                            == self
                                                .resolve_operand_ref(program, *expected)
                                                .as_symbol()
                                                .expect("validated match heading is symbolic")
                                    },
                                )
                                && program.map_entries(*equalities).iter().all(
                                    |(column, expected)| {
                                        let column = self
                                            .resolve_operand_ref(program, *column)
                                            .as_symbol()
                                            .expect("validated match column is symbolic");
                                        let Some(position) = value.column_position(column) else {
                                            return false;
                                        };
                                        value.rows().first().is_some_and(|row| {
                                            row.values()[position]
                                                == self.resolve_operand_ref(program, *expected)
                                        })
                                    },
                                )
                        })
                        .unwrap_or(false);
                self.write_register_unchecked(*dst, Value::bool(matched));
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::RelationCell {
                dst,
                relation,
                column,
            } => {
                let relation_value = self.read_register_unchecked(*relation).clone();
                let Some(value) = relation_value
                    .with_relation(|value| {
                        let position = value.column_position(*column)?;
                        value.rows().first()?.values().get(position).cloned()
                    })
                    .flatten()
                else {
                    return self.begin_raise(Value::error(
                        Symbol::intern("E_MATCH"),
                        Some("relation cell load requires a matched row"),
                        Some(relation_value),
                    ));
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::RelationCellAt {
                dst,
                relation,
                index,
                heading,
                column,
            } => {
                let relation_value = self.read_register_unchecked(*relation).clone();
                let row_index = self
                    .read_register_unchecked(*index)
                    .as_int()
                    .and_then(|index| usize::try_from(index).ok());
                let expected_heading = program.operands(*heading);
                let value = relation_value
                    .with_relation(|value| {
                        if value.heading().len() != expected_heading.len()
                            || !value.heading().iter().zip(expected_heading).all(
                                |(actual, expected)| {
                                    *actual
                                        == self
                                            .resolve_operand_ref(program, *expected)
                                            .as_symbol()
                                            .expect("validated row heading is symbolic")
                                },
                            )
                        {
                            return None;
                        }
                        let position = value.column_position(*column)?;
                        value
                            .rows()
                            .get(row_index?)?
                            .values()
                            .get(position)
                            .cloned()
                    })
                    .flatten();
                let Some(value) = value else {
                    return self.begin_raise(Value::error(
                        Symbol::intern("E_MATCH"),
                        Some("relation row does not match the loop pattern"),
                        Some(relation_value),
                    ));
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ScanExists {
                dst,
                relation,
                bindings,
            } => {
                let relation = program.relation(*relation);
                require_read(host.authority(), relation)?;
                let bindings = self.resolve_bindings(program, program.bindings(*bindings));
                let exists = match host.scan_relation(relation, &bindings) {
                    Ok(rows) => !rows.is_empty(),
                    Err(error) => return self.raise_kernel_error(error),
                };
                self.write_register_unchecked(*dst, Value::bool(exists));
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ScanBindings {
                dst,
                relation,
                bindings,
                outputs,
            } => {
                let relation = program.relation(*relation);
                require_read(host.authority(), relation)?;
                let bindings = self.resolve_bindings(program, program.bindings(*bindings));
                let rows = match host.scan_relation(relation, &bindings) {
                    Ok(rows) => rows,
                    Err(error) => return self.raise_kernel_error(error),
                };
                let outputs = program.query_bindings(*outputs);
                let result = query_relation(rows, outputs, bindings.len())?;
                self.write_register_unchecked(*dst, result);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ScanValue { dst, relation, key } => {
                let relation = program.relation(*relation);
                require_read(host.authority(), relation)?;
                let key = self.resolve_operand_ref(program, *key);
                let rows = match host.scan_relation(relation, &[Some(key), None]) {
                    Ok(rows) => rows,
                    Err(error) => return self.raise_kernel_error(error),
                };
                if rows.len() != 1 {
                    return self.begin_raise(Value::error(
                        Symbol::intern("E_CARDINALITY"),
                        Some("functional dot read requires exactly one fact"),
                        Some(Value::int(i64::try_from(rows.len()).unwrap_or(i64::MAX)).unwrap()),
                    ));
                }
                let value = rows[0].values()[1].clone();
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::Assert { relation, values } => {
                let relation = program.relation(*relation);
                require_write(host.authority(), relation)?;
                host.assert_tuple(
                    relation,
                    self.resolve_tuple(program, program.operands(*values)),
                )?;
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::Retract { relation, values } => {
                let relation = program.relation(*relation);
                require_write(host.authority(), relation)?;
                host.retract_tuple(
                    relation,
                    self.resolve_tuple(program, program.operands(*values)),
                )?;
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::RetractWhere { relation, bindings } => {
                let relation = program.relation(*relation);
                require_read(host.authority(), relation)?;
                require_write(host.authority(), relation)?;
                let bindings = self.resolve_bindings(program, program.bindings(*bindings));
                host.retract_matching(relation, &bindings)?;
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ScanDynamic {
                dst,
                relation,
                args,
            } => {
                let relation = program.relation(*relation);
                require_read(host.authority(), relation)?;
                let (bindings, outputs) =
                    self.resolve_relation_bindings(program, program.relation_args(*args))?;
                let rows = match host.scan_relation(relation, &bindings) {
                    Ok(rows) => rows,
                    Err(error) => return self.raise_kernel_error(error),
                };
                let value = if outputs.is_empty() {
                    Value::bool(!rows.is_empty())
                } else {
                    query_relation(rows, &outputs, bindings.len())?
                };
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::AssertDynamic { relation, args } => {
                let relation = program.relation(*relation);
                require_write(host.authority(), relation)?;
                host.assert_tuple(
                    relation,
                    Tuple::new(
                        self.resolve_relation_values(program, program.relation_args(*args))?,
                    ),
                )?;
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::RetractDynamic { relation, args } => {
                let relation = program.relation(*relation);
                require_write(host.authority(), relation)?;
                let (bindings, _) =
                    self.resolve_relation_bindings(program, program.relation_args(*args))?;
                if bindings.iter().any(Option::is_none) {
                    require_read(host.authority(), relation)?;
                    host.retract_matching(relation, &bindings)?;
                } else {
                    host.retract_tuple(relation, Tuple::new(bindings.into_iter().flatten()))?;
                }
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ReplaceFunctional { relation, values } => {
                let relation = program.relation(*relation);
                require_write(host.authority(), relation)?;
                host.replace_functional_tuple(
                    relation,
                    self.resolve_tuple(program, program.operands(*values)),
                )?;
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::Jump { target } => {
                self.current_frame_mut_unchecked().ip = target.0 as usize;
                Ok(VmHostResponse::Continue)
            }
            Opcode::EnterTry {
                catches,
                finally,
                end,
            } => {
                let catches = program
                    .catches(*catches)
                    .iter()
                    .map(|catch| CatchHandler {
                        code: catch.code.map(|id| program.constant(id).clone()),
                        binding: catch.binding,
                        target: catch.target.0 as usize,
                    })
                    .collect();
                self.current_frame_mut_unchecked()
                    .try_stack
                    .push(TryRegion {
                        catches,
                        finally: finally.map(|target| target.0 as usize),
                        end: end.0 as usize,
                    });
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::ExitTry => self.exit_try_region(),
            Opcode::EndFinally => self.end_finally(),
            Opcode::Emit { target, value } => {
                let target_value = self.resolve_operand_ref(program, *target);
                let target = target_value
                    .as_identity()
                    .ok_or(RuntimeError::InvalidEffectTarget(target_value))?;
                let value = self.resolve_operand_ref(program, *value);
                host.emit(target, value)?;
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::LoadFunction {
                dst,
                program: callee,
                captures,
                min_arity,
                max_arity,
            } => {
                let callee = program.program(*callee);
                let callee_id = self.intern_program(Arc::clone(callee));
                let captures = self.resolve_operands(program, program.operands(*captures));
                let function = self.intern_function(CallableInfo {
                    program: callee_id,
                    captures,
                    min_arity: *min_arity as usize,
                    max_arity: (*max_arity != u16::MAX).then_some(*max_arity as usize),
                })?;
                self.write_register_unchecked(*dst, Value::function(function));
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::CallValue { dst, callee, args } => {
                let callee = self.resolve_operand_ref(program, *callee);
                let user_args = self.resolve_operands(program, program.operands(*args));
                self.call_function_value(*dst, callee, user_args, max_call_depth)?;
                Ok(VmHostResponse::Continue)
            }
            Opcode::CallValueDynamic { dst, callee, args } => {
                let callee = self.resolve_operand_ref(program, *callee);
                let user_args = self.resolve_list_items(program, program.list_items(*args))?;
                self.call_function_value(*dst, callee, user_args, max_call_depth)?;
                Ok(VmHostResponse::Continue)
            }
            Opcode::BuiltinCallDynamic {
                dst,
                name,
                result_kind,
                args,
            } => {
                let args = self.resolve_list_items(program, program.list_items(*args))?;
                let value = match host.call_builtin(*name, &args) {
                    Ok(value) => value,
                    Err(RuntimeError::Raised(error)) => return self.begin_raise(error),
                    Err(error) => return Err(error),
                };
                let value = validate_builtin_result(*name, *result_kind, value)?;
                self.write_register_unchecked(*dst, value);
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Continue)
            }
            Opcode::Dispatch {
                dst,
                spec,
                selector,
                roles,
            } => {
                if self.state.frames.len() >= max_call_depth {
                    return Err(RuntimeError::MaxCallDepthExceeded {
                        max_depth: max_call_depth,
                    });
                }
                let selector = self.resolve_operand_ref(program, *selector);
                let mut roles = program
                    .roles(*roles)
                    .iter()
                    .map(|(role, value)| {
                        (
                            program.constant(*role).clone(),
                            self.resolve_operand_ref(program, *value),
                        )
                    })
                    .collect::<Vec<_>>();
                normalize_dispatch_roles(&mut roles);
                let spec = program.dispatch_spec(*spec);
                let methods = applicable_method_calls_normalized(
                    host,
                    spec.relations,
                    selector.clone(),
                    &roles,
                )?;
                let entry =
                    select_authorized_method_call(host.authority(), selector.clone(), methods)?;
                let method = &entry.method;
                let program_id = method_program_id(host, spec.program_relation, method)?
                    .ok_or_else(|| RuntimeError::MissingMethodProgram {
                        method: method.clone(),
                    })?;
                let callee_id = self.resolve_program_id(host, spec.program_bytes, &program_id)?;
                let register_count = self.program_unchecked(callee_id).register_count();
                let args = entry.args.ok_or_else(|| {
                    RuntimeError::ProgramArtifact("method parameter position is invalid".to_owned())
                })?;
                self.advance_ip_unchecked();
                self.state
                    .frames
                    .push(Frame::new(callee_id, register_count, Some(*dst), args)?);
                Ok(VmHostResponse::Continue)
            }
            Opcode::DynamicDispatch {
                dst,
                spec,
                selector,
                roles,
            } => {
                if self.state.frames.len() >= max_call_depth {
                    return Err(RuntimeError::MaxCallDepthExceeded {
                        max_depth: max_call_depth,
                    });
                }
                let selector = self.resolve_operand_ref(program, *selector);
                let roles = self.resolve_operand_ref(program, *roles);
                let mut roles = dynamic_dispatch_roles(&roles)?;
                normalize_dispatch_roles(&mut roles);
                let spec = program.dispatch_spec(*spec);
                let methods = applicable_method_calls_normalized(
                    host,
                    spec.relations,
                    selector.clone(),
                    &roles,
                )?;
                let entry =
                    select_authorized_method_call(host.authority(), selector.clone(), methods)?;
                let method = &entry.method;
                let program_id = method_program_id(host, spec.program_relation, method)?
                    .ok_or_else(|| RuntimeError::MissingMethodProgram {
                        method: method.clone(),
                    })?;
                let callee_id = self.resolve_program_id(host, spec.program_bytes, &program_id)?;
                let register_count = self.program_unchecked(callee_id).register_count();
                let args = entry.args.ok_or_else(|| {
                    RuntimeError::ProgramArtifact("method parameter position is invalid".to_owned())
                })?;
                self.advance_ip_unchecked();
                self.state
                    .frames
                    .push(Frame::new(callee_id, register_count, Some(*dst), args)?);
                Ok(VmHostResponse::Continue)
            }
            Opcode::PositionalDispatch {
                dst,
                spec,
                selector,
                args,
            } => {
                if self.state.frames.len() >= max_call_depth {
                    return Err(RuntimeError::MaxCallDepthExceeded {
                        max_depth: max_call_depth,
                    });
                }
                let selector = self.resolve_operand_ref(program, *selector);
                let args = self.resolve_operands(program, program.operands(*args));
                let spec = program.dispatch_spec(*spec);
                let methods = applicable_positional_methods_cached(
                    host,
                    spec.relations,
                    selector.clone(),
                    &args,
                )?;
                let method =
                    select_authorized_method(host.authority(), selector.clone(), &methods)?;
                let program_id = method_program_id(host, spec.program_relation, &method)?
                    .ok_or_else(|| RuntimeError::MissingMethodProgram {
                        method: method.clone(),
                    })?;
                let callee_id = self.resolve_program_id(host, spec.program_bytes, &program_id)?;
                let register_count = self.program_unchecked(callee_id).register_count();
                self.advance_ip_unchecked();
                self.state
                    .frames
                    .push(Frame::new(callee_id, register_count, Some(*dst), args)?);
                Ok(VmHostResponse::Continue)
            }
            Opcode::PositionalDispatchDynamic {
                dst,
                spec,
                selector,
                args,
            } => {
                if self.state.frames.len() >= max_call_depth {
                    return Err(RuntimeError::MaxCallDepthExceeded {
                        max_depth: max_call_depth,
                    });
                }
                let selector = self.resolve_operand_ref(program, *selector);
                let args = self.resolve_list_items(program, program.list_items(*args))?;
                let spec = program.dispatch_spec(*spec);
                let methods = applicable_positional_methods_cached(
                    host,
                    spec.relations,
                    selector.clone(),
                    &args,
                )?;
                let method =
                    select_authorized_method(host.authority(), selector.clone(), &methods)?;
                let program_id = method_program_id(host, spec.program_relation, &method)?
                    .ok_or_else(|| RuntimeError::MissingMethodProgram {
                        method: method.clone(),
                    })?;
                let callee_id = self.resolve_program_id(host, spec.program_bytes, &program_id)?;
                let register_count = self.program_unchecked(callee_id).register_count();
                self.advance_ip_unchecked();
                self.state
                    .frames
                    .push(Frame::new(callee_id, register_count, Some(*dst), args)?);
                Ok(VmHostResponse::Continue)
            }
            Opcode::SpawnDispatch {
                dst,
                selector,
                roles,
                delay,
            } => {
                let selector = self.resolve_operand_ref(program, *selector);
                let selector_symbol = selector
                    .as_symbol()
                    .ok_or_else(|| RuntimeError::InvalidSpawnSelector(selector.clone()))?;
                let mut spawn_roles = Vec::new();
                for (role, value) in program.roles(*roles) {
                    let role = program.constant(*role).clone();
                    let role_symbol = role
                        .as_symbol()
                        .ok_or_else(|| RuntimeError::InvalidSpawnRole(role.clone()))?;
                    spawn_roles.push((role_symbol, self.resolve_operand_ref(program, *value)));
                }
                let delay_millis = delay
                    .map(|delay| {
                        self.suspend_duration(self.resolve_operand_ref(program, delay))
                            .and_then(|kind| match kind {
                                SuspendKind::TimedMillis(millis) => Ok(millis),
                                _ => unreachable!(),
                            })
                    })
                    .transpose()?;
                normalize_spawn_roles(&mut spawn_roles);
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Spawn(SpawnRequest {
                    selector: selector_symbol,
                    target: SpawnTarget::NamedRoles(spawn_roles),
                    delay_millis,
                }))
            }
            Opcode::SpawnDispatchDynamic {
                dst,
                selector,
                roles,
                delay,
            } => {
                let selector = self.resolve_operand_ref(program, *selector);
                let selector_symbol = selector
                    .as_symbol()
                    .ok_or_else(|| RuntimeError::InvalidSpawnSelector(selector.clone()))?;
                let roles = self.resolve_operand_ref(program, *roles);
                let mut spawn_roles = dynamic_spawn_roles(&roles)?;
                let delay_millis = delay
                    .map(|delay| {
                        self.suspend_duration(self.resolve_operand_ref(program, delay))
                            .and_then(|kind| match kind {
                                SuspendKind::TimedMillis(millis) => Ok(millis),
                                _ => unreachable!(),
                            })
                    })
                    .transpose()?;
                normalize_spawn_roles(&mut spawn_roles);
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Spawn(SpawnRequest {
                    selector: selector_symbol,
                    target: SpawnTarget::NamedRoles(spawn_roles),
                    delay_millis,
                }))
            }
            Opcode::SpawnPositionalDispatch {
                dst,
                selector,
                args,
                delay,
            } => {
                let selector = self.resolve_operand_ref(program, *selector);
                let selector_symbol = selector
                    .as_symbol()
                    .ok_or_else(|| RuntimeError::InvalidSpawnSelector(selector.clone()))?;
                let args = self.resolve_operands(program, program.operands(*args));
                let delay_millis = delay
                    .map(|delay| {
                        self.suspend_duration(self.resolve_operand_ref(program, delay))
                            .and_then(|kind| match kind {
                                SuspendKind::TimedMillis(millis) => Ok(millis),
                                _ => unreachable!(),
                            })
                    })
                    .transpose()?;
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Spawn(SpawnRequest {
                    selector: selector_symbol,
                    target: SpawnTarget::PositionalArgs(args),
                    delay_millis,
                }))
            }
            Opcode::SpawnPositionalDispatchDynamic {
                dst,
                selector,
                args,
                delay,
            } => {
                let selector = self.resolve_operand_ref(program, *selector);
                let selector_symbol = selector
                    .as_symbol()
                    .ok_or_else(|| RuntimeError::InvalidSpawnSelector(selector.clone()))?;
                let args = self.resolve_list_items(program, program.list_items(*args))?;
                let delay_millis = delay
                    .map(|delay| {
                        self.suspend_duration(self.resolve_operand_ref(program, delay))
                            .and_then(|kind| match kind {
                                SuspendKind::TimedMillis(millis) => Ok(millis),
                                _ => unreachable!(),
                            })
                    })
                    .transpose()?;
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Spawn(SpawnRequest {
                    selector: selector_symbol,
                    target: SpawnTarget::PositionalArgs(args),
                    delay_millis,
                }))
            }
            Opcode::Commit => {
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Commit)
            }
            Opcode::Suspend { kind } => {
                self.advance_ip_unchecked();
                Ok(VmHostResponse::Suspend(program.suspend_kind(*kind).clone()))
            }
            Opcode::SuspendValue { dst, duration } => {
                let kind = duration
                    .map(|duration| {
                        self.suspend_duration(self.resolve_operand_ref(program, duration))
                    })
                    .transpose()?
                    .unwrap_or(SuspendKind::Never);
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Suspend(kind))
            }
            Opcode::CommitValue { dst } => {
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Suspend(SuspendKind::Commit))
            }
            Opcode::Read { dst, metadata } => {
                let metadata = metadata
                    .map(|metadata| self.resolve_operand_ref(program, metadata))
                    .unwrap_or_else(Value::nothing);
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Suspend(SuspendKind::WaitingForInput(
                    metadata,
                )))
            }
            Opcode::MailboxRecv {
                dst,
                receivers,
                timeout,
            } => {
                let receivers_value = self.resolve_operand_ref(program, *receivers);
                let Some(receivers) = receivers_value.with_list(<[Value]>::to_vec) else {
                    return Err(RuntimeError::InvalidBuiltinCall {
                        name: Symbol::intern("mailbox_recv"),
                        message: "mailbox_recv expects a list of receive caps".to_owned(),
                    });
                };
                if receivers.is_empty() {
                    return Err(RuntimeError::InvalidBuiltinCall {
                        name: Symbol::intern("mailbox_recv"),
                        message: "mailbox_recv expects at least one receive cap".to_owned(),
                    });
                }
                for receiver in &receivers {
                    host.validate_mailbox_receiver(receiver)?;
                }
                let timeout_millis = timeout
                    .map(|timeout| {
                        self.suspend_duration(self.resolve_operand_ref(program, timeout))
                            .and_then(|kind| match kind {
                                SuspendKind::TimedMillis(millis) => Ok(millis),
                                _ => unreachable!(),
                            })
                    })
                    .transpose()?;
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Suspend(SuspendKind::MailboxRecv(
                    MailboxRecvRequest {
                        receivers,
                        timeout_millis,
                    },
                )))
            }
            Opcode::ExternalRequest {
                dst,
                service,
                payload,
                timeout,
            } => {
                let service_value = self.resolve_operand_ref(program, *service);
                let Some(service) = service_value.as_symbol() else {
                    return Err(RuntimeError::InvalidBuiltinCall {
                        name: Symbol::intern("external_request"),
                        message: "external_request expects a symbol service".to_owned(),
                    });
                };
                if !host.authority().can_effect() {
                    return Err(RuntimeError::PermissionDenied {
                        operation: "external_request",
                        target: service_value,
                    });
                }
                let payload = self.resolve_operand_ref(program, *payload);
                let timeout_millis = timeout
                    .map(|timeout| {
                        self.suspend_duration(self.resolve_operand_ref(program, timeout))
                            .and_then(|kind| match kind {
                                SuspendKind::TimedMillis(millis) => Ok(millis),
                                _ => unreachable!(),
                            })
                    })
                    .transpose()?;
                self.advance_ip_unchecked();
                self.state.pending_resume = Some(*dst);
                Ok(VmHostResponse::Suspend(SuspendKind::ExternalRequest(
                    ExternalRequest {
                        service,
                        payload,
                        timeout_millis,
                    },
                )))
            }
            Opcode::RollbackRetry => {
                self.advance_ip_unchecked();
                Ok(VmHostResponse::RollbackRetry)
            }
            Opcode::Abort { error } => {
                let error = self.resolve_operand_ref(program, *error);
                Ok(VmHostResponse::Abort(error))
            }
            Opcode::Raise {
                error,
                message,
                value,
            } => {
                let error = self.resolve_operand_ref(program, *error);
                let message = message.map(|message| self.resolve_operand_ref(program, message));
                let value = value.map(|value| self.resolve_operand_ref(program, value));
                let error = normalize_raised_error(error, message, value)?;
                self.begin_raise(error)
            }
        }
    }

    fn return_from_frame(&mut self, value: Value) -> Result<VmHostResponse, RuntimeError> {
        {
            let frame = self.current_frame_mut_unchecked();
            if frame.pending_finally.pop().is_some() {
                // A return from inside a finally body replaces the control flow
                // that originally entered the finally.
            }
            while let Some(region) = frame.try_stack.pop() {
                if let Some(finally) = region.finally {
                    frame
                        .pending_finally
                        .push(FinallyContinuation::Return(value));
                    frame.ip = finally;
                    return Ok(VmHostResponse::Continue);
                }
            }
        }

        let frame = self
            .state
            .frames
            .pop()
            .ok_or(RuntimeError::EmptyCallStack)?;
        let Some(return_register) = frame.return_register else {
            return Ok(VmHostResponse::Complete(value));
        };
        self.write_register_unchecked(return_register, value);
        Ok(VmHostResponse::Continue)
    }

    fn exit_try_region(&mut self) -> Result<VmHostResponse, RuntimeError> {
        let frame = self.current_frame_mut()?;
        let region = frame.try_stack.pop().ok_or(RuntimeError::EmptyTryStack)?;
        if let Some(finally) = region.finally {
            frame
                .pending_finally
                .push(FinallyContinuation::Normal(region.end));
            frame.ip = finally;
        } else {
            frame.ip = region.end;
        }
        Ok(VmHostResponse::Continue)
    }

    fn end_finally(&mut self) -> Result<VmHostResponse, RuntimeError> {
        let continuation = self
            .current_frame_mut()?
            .pending_finally
            .pop()
            .ok_or(RuntimeError::EmptyTryStack)?;
        match continuation {
            FinallyContinuation::Normal(target) => {
                self.current_frame_mut_unchecked().ip = target;
                Ok(VmHostResponse::Continue)
            }
            FinallyContinuation::Raise(error) => self.begin_raise(error),
            FinallyContinuation::Return(value) => self.return_from_frame(value),
        }
    }

    fn begin_raise(&mut self, error: Value) -> Result<VmHostResponse, RuntimeError> {
        loop {
            let Some(frame) = self.state.frames.last_mut() else {
                return Ok(VmHostResponse::Abort(error));
            };

            if frame.pending_finally.pop().is_some() {
                // A raise from inside a finally body replaces the control flow
                // that originally entered the finally.
            }

            while let Some(region) = frame.try_stack.pop() {
                if let Some(handler) = matching_handler(&region.catches, &error) {
                    if let Some(binding) = handler.binding {
                        let register_count = frame.registers.len();
                        let slot = frame.registers.get_mut(binding.0 as usize).ok_or(
                            RuntimeError::RegisterOutOfBounds {
                                register: binding.0,
                                register_count,
                            },
                        )?;
                        *slot = error;
                    }
                    if let Some(finally) = region.finally {
                        frame.try_stack.push(TryRegion {
                            catches: Vec::new(),
                            finally: Some(finally),
                            end: region.end,
                        });
                    }
                    frame.ip = handler.target;
                    return Ok(VmHostResponse::Continue);
                }

                if let Some(finally) = region.finally {
                    frame
                        .pending_finally
                        .push(FinallyContinuation::Raise(error));
                    frame.ip = finally;
                    return Ok(VmHostResponse::Continue);
                }
            }

            self.state.frames.pop();
        }
    }

    fn advance_ip_unchecked(&mut self) {
        self.current_frame_mut_unchecked().ip += 1;
    }

    /// Convert a KernelError into a raised Mica error value so that
    /// `try/catch` can handle it. Without this, scan errors from
    /// computed relations propagate as RuntimeErrors that crash the
    /// task instead of being catchable.
    fn raise_kernel_error(
        &mut self,
        error: mica_relation_kernel::KernelError,
    ) -> Result<VmHostResponse, RuntimeError> {
        let message = format!("{error:?}");
        let error_value = Value::error(Symbol::intern("E_DB"), Some(message), None);
        self.begin_raise(error_value)
    }

    fn intern_program(&mut self, program: Arc<Program>) -> usize {
        if let Some((index, _)) = self
            .state
            .programs
            .iter()
            .enumerate()
            .find(|(_, loaded)| Arc::ptr_eq(loaded, &program))
        {
            return index;
        }
        let index = self.state.programs.len();
        self.state.programs.push(program);
        index
    }

    fn intern_function(&mut self, function: CallableInfo) -> Result<FunctionId, RuntimeError> {
        if let Some((index, _)) = self
            .state
            .functions
            .iter()
            .enumerate()
            .find(|(_, loaded)| **loaded == function)
        {
            return FunctionId::new(index as u64).ok_or_else(|| {
                RuntimeError::ProgramArtifact("function id exceeds value payload range".to_owned())
            });
        }
        let index = self.state.functions.len();
        let function_id = FunctionId::new(index as u64).ok_or_else(|| {
            RuntimeError::ProgramArtifact("function id exceeds value payload range".to_owned())
        })?;
        self.state.functions.push(function);
        Ok(function_id)
    }

    fn callable(&self, id: FunctionId) -> Result<CallableInfo, RuntimeError> {
        self.state
            .functions
            .get(id.raw() as usize)
            .cloned()
            .ok_or(RuntimeError::InvalidFunction(id.raw()))
    }

    fn call_function_value(
        &mut self,
        dst: Register,
        callee: Value,
        user_args: Vec<Value>,
        max_call_depth: usize,
    ) -> Result<(), RuntimeError> {
        if self.state.frames.len() >= max_call_depth {
            return Err(RuntimeError::MaxCallDepthExceeded {
                max_depth: max_call_depth,
            });
        }
        let function = callee
            .as_function()
            .ok_or_else(|| RuntimeError::InvalidCallable(callee.clone()))?;
        let callable = self.callable(function)?;
        if user_args.len() < callable.min_arity {
            return Err(RuntimeError::InvalidCallArity {
                expected_min: callable.min_arity,
                expected_max: callable.max_arity.unwrap_or(usize::MAX),
                actual: user_args.len(),
            });
        }
        if let Some(max_arity) = callable.max_arity
            && user_args.len() > max_arity
        {
            return Err(RuntimeError::InvalidCallArity {
                expected_min: callable.min_arity,
                expected_max: max_arity,
                actual: user_args.len(),
            });
        }
        let register_count = self.program_unchecked(callable.program).register_count();
        let mut args = callable.captures;
        args.push(Value::list(user_args));
        self.advance_ip_unchecked();
        self.state.frames.push(Frame::new(
            callable.program,
            register_count,
            Some(dst),
            args,
        )?);
        Ok(())
    }

    fn resolve_program_id<H: VmHost>(
        &mut self,
        host: &mut H,
        program_bytes_relation: RelationId,
        program_id: &Value,
    ) -> Result<usize, RuntimeError> {
        if let Some((_, index)) = self
            .state
            .resolved_programs
            .iter()
            .find(|(resolved, _)| resolved == program_id)
        {
            return Ok(*index);
        }
        let program = host.resolve_program(program_bytes_relation, program_id)?;
        let index = self.intern_program(program);
        self.state
            .resolved_programs
            .push((program_id.clone(), index));
        Ok(index)
    }

    fn program_unchecked(&self, program: usize) -> &Program {
        self.state.programs[program].as_ref()
    }

    fn current_frame(&self) -> Result<&Frame, RuntimeError> {
        self.state.frames.last().ok_or(RuntimeError::EmptyCallStack)
    }

    fn current_frame_unchecked(&self) -> &Frame {
        debug_assert!(!self.state.frames.is_empty());
        self.state.frames.last().unwrap()
    }

    fn current_frame_mut(&mut self) -> Result<&mut Frame, RuntimeError> {
        self.state
            .frames
            .last_mut()
            .ok_or(RuntimeError::EmptyCallStack)
    }

    fn current_frame_mut_unchecked(&mut self) -> &mut Frame {
        debug_assert!(!self.state.frames.is_empty());
        self.state.frames.last_mut().unwrap()
    }

    fn read_register_unchecked(&self, register: Register) -> &Value {
        let frame = self
            .state
            .frames
            .last()
            .expect("validated VM execution requires a current frame");
        debug_assert!((register.0 as usize) < frame.registers.len());
        &frame.registers[register.0 as usize]
    }

    fn write_register(&mut self, register: Register, value: Value) -> Result<(), RuntimeError> {
        let frame = self.current_frame_mut()?;
        let register_count = frame.registers.len();
        let slot = frame.registers.get_mut(register.0 as usize).ok_or(
            RuntimeError::RegisterOutOfBounds {
                register: register.0,
                register_count,
            },
        )?;
        *slot = value;
        Ok(())
    }

    fn write_register_unchecked(&mut self, register: Register, value: Value) {
        let frame = self.current_frame_mut_unchecked();
        debug_assert!((register.0 as usize) < frame.registers.len());
        frame.registers[register.0 as usize] = value;
    }

    #[inline]
    fn resolve_operand_ref(&self, program: &Program, operand: OperandRef) -> Value {
        match operand {
            OperandRef::Register(register) => self.read_register_unchecked(register).clone(),
            OperandRef::Constant(id) => program.constant(id).clone(),
        }
    }

    #[inline]
    fn resolve_operands(&self, program: &Program, operands: &[OperandRef]) -> Vec<Value> {
        operands
            .iter()
            .map(|operand| self.resolve_operand_ref(program, *operand))
            .collect()
    }

    #[inline]
    fn build_list(&self, program: &Program, items: &[CompactListItem]) -> Value {
        let mut values = Vec::new();
        for item in items {
            match item {
                CompactListItem::Value(operand) => {
                    values.push(self.resolve_operand_ref(program, *operand));
                }
                CompactListItem::Splice(operand) => {
                    let splice = self.resolve_operand_ref(program, *operand);
                    let Some(()) = splice.with_list(|items| {
                        values.extend(items.iter().cloned());
                    }) else {
                        return Value::nothing();
                    };
                }
            }
        }
        Value::list(values)
    }

    fn resolve_list_items(
        &self,
        program: &Program,
        items: &[CompactListItem],
    ) -> Result<Vec<Value>, RuntimeError> {
        let mut values = Vec::new();
        for item in items {
            match item {
                CompactListItem::Value(operand) => {
                    values.push(self.resolve_operand_ref(program, *operand));
                }
                CompactListItem::Splice(operand) => {
                    let splice = self.resolve_operand_ref(program, *operand);
                    let Some(()) = splice.with_list(|items| {
                        values.extend(items.iter().cloned());
                    }) else {
                        return Err(RuntimeError::InvalidArgumentSplice(splice));
                    };
                }
            }
        }
        Ok(values)
    }

    fn resolve_map_items(
        &self,
        program: &Program,
        items: &[CompactMapItem],
    ) -> Result<Value, RuntimeError> {
        let mut entries = Vec::new();
        for item in items {
            match item {
                CompactMapItem::Entry(key, value) => {
                    entries.push((
                        self.resolve_operand_ref(program, *key),
                        self.resolve_operand_ref(program, *value),
                    ));
                }
                CompactMapItem::Splice(operand) => {
                    let splice = self.resolve_operand_ref(program, *operand);
                    let Some(()) = splice.with_map(|items| {
                        entries.extend(items.iter().cloned());
                    }) else {
                        return Err(RuntimeError::InvalidArgumentSplice(splice));
                    };
                }
            }
        }
        Ok(Value::map(entries))
    }

    #[inline]
    fn resolve_tuple(&self, program: &Program, values: &[OperandRef]) -> Tuple {
        Tuple::new(self.resolve_operands(program, values))
    }

    #[inline]
    fn resolve_bindings(
        &self,
        program: &Program,
        bindings: &[Option<OperandRef>],
    ) -> Vec<Option<Value>> {
        bindings
            .iter()
            .map(|binding| binding.map(|operand| self.resolve_operand_ref(program, operand)))
            .collect()
    }

    fn resolve_relation_values(
        &self,
        program: &Program,
        args: &[CompactRelationArg],
    ) -> Result<Vec<Value>, RuntimeError> {
        let mut values = Vec::new();
        self.visit_relation_args(program, args, |arg| {
            match arg {
                ExpandedRelationArg::Value(value) => values.push(value),
                ExpandedRelationArg::Query(_) | ExpandedRelationArg::Hole => {
                    return Err(RuntimeError::InvalidBuiltinCall {
                        name: Symbol::intern("relation"),
                        message: "relation value operation cannot contain query variables or holes"
                            .to_owned(),
                    });
                }
            }
            Ok(())
        })?;
        Ok(values)
    }

    fn resolve_relation_bindings(
        &self,
        program: &Program,
        args: &[CompactRelationArg],
    ) -> Result<(Vec<Option<Value>>, Vec<QueryBinding>), RuntimeError> {
        let mut bindings = Vec::new();
        let mut outputs = Vec::new();
        self.visit_relation_args(program, args, |arg| {
            match arg {
                ExpandedRelationArg::Value(value) => bindings.push(Some(value)),
                ExpandedRelationArg::Query(name) => {
                    let position = u16::try_from(bindings.len()).map_err(|_| {
                        RuntimeError::RelationArgumentCountExceeded {
                            count: bindings.len(),
                        }
                    })?;
                    outputs.push(QueryBinding { name, position });
                    bindings.push(None);
                }
                ExpandedRelationArg::Hole => bindings.push(None),
            }
            Ok(())
        })?;
        Ok((bindings, outputs))
    }

    fn visit_relation_args(
        &self,
        program: &Program,
        args: &[CompactRelationArg],
        mut visit: impl FnMut(ExpandedRelationArg) -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        for arg in args {
            match arg {
                CompactRelationArg::Value(operand) => {
                    visit(ExpandedRelationArg::Value(
                        self.resolve_operand_ref(program, *operand),
                    ))?;
                }
                CompactRelationArg::Splice(operand) => {
                    let splice = self.resolve_operand_ref(program, *operand);
                    let Some(result) = splice.with_list(|items| {
                        for item in items {
                            visit(ExpandedRelationArg::Value(item.clone()))?;
                        }
                        Ok::<(), RuntimeError>(())
                    }) else {
                        return Err(RuntimeError::InvalidRelationSplice(splice));
                    };
                    result?;
                }
                CompactRelationArg::Query(name) => visit(ExpandedRelationArg::Query(*name))?,
                CompactRelationArg::Hole => visit(ExpandedRelationArg::Hole)?,
            }
        }
        Ok(())
    }

    fn suspend_duration(&self, value: Value) -> Result<SuspendKind, RuntimeError> {
        let seconds = if let Some(seconds) = value.as_int() {
            seconds as f64
        } else if let Some(seconds) = value.as_float() {
            seconds as f64
        } else {
            return Err(RuntimeError::InvalidSuspendDuration(value));
        };
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(RuntimeError::InvalidSuspendDuration(value));
        }
        let millis = (seconds * 1_000.0).round();
        if millis > u64::MAX as f64 {
            return Err(RuntimeError::InvalidSuspendDuration(value));
        }
        Ok(SuspendKind::TimedMillis(millis as u64))
    }
}

fn truthy(value: &Value) -> bool {
    match value.kind() {
        ValueKind::Bool => value.as_bool().unwrap_or(false),
        ValueKind::List => value.list_len().is_some_and(|len| len > 0),
        ValueKind::Relation => value
            .with_relation(|relation| !relation.is_empty())
            .unwrap(),
        _ => true,
    }
}

#[cfg(feature = "cranelift")]
fn native_integer_loop_is_profitable(
    current: i64,
    step: i64,
    limit: i64,
    max_iterations: usize,
) -> bool {
    if max_iterations < MIN_NATIVE_INTEGER_LOOP_ITERATIONS {
        return false;
    }
    if step <= 0 {
        return current < limit;
    }
    let distance = i128::from(limit) - i128::from(current);
    if distance <= 0 {
        return false;
    }
    let step = i128::from(step);
    let iterations = (distance + step - 1) / step;
    iterations >= MIN_NATIVE_INTEGER_LOOP_ITERATIONS as i128
}

#[cfg(feature = "cranelift")]
fn native_float_loop_is_profitable(
    current: f32,
    step: f32,
    limit: f32,
    plan: FloatLoopPlan,
    max_iterations: usize,
) -> bool {
    if max_iterations < MIN_NATIVE_FLOAT_LOOP_ITERATIONS {
        return false;
    }
    let next = match plan.arithmetic {
        FloatArithmetic::Add => current + step,
        FloatArithmetic::Subtract => current - step,
        FloatArithmetic::Multiply => current * step,
        FloatArithmetic::Divide => current / step,
    };
    if !next.is_finite() || !float_comparison_holds(next, limit, plan.comparison) {
        return false;
    }
    if next == current {
        return true;
    }
    if plan.comparison == FloatComparison::Equal {
        return next == limit;
    }

    let progress = match plan.arithmetic {
        FloatArithmetic::Add => f64::from(step),
        FloatArithmetic::Subtract => -f64::from(step),
        FloatArithmetic::Multiply if current > 0.0 && limit > 0.0 && step > 0.0 => {
            let factor = f64::from(step);
            return multiplicative_float_loop_is_profitable(
                current,
                limit,
                factor,
                plan.comparison,
            );
        }
        FloatArithmetic::Divide if current > 0.0 && limit > 0.0 && step > 0.0 && step != 1.0 => {
            let factor = 1.0 / f64::from(step);
            return multiplicative_float_loop_is_profitable(
                current,
                limit,
                factor,
                plan.comparison,
            );
        }
        _ => f64::from(next) - f64::from(current),
    };
    let distance = f64::from(limit) - f64::from(current);
    if distance.signum() != progress.signum() {
        return true;
    }
    (distance / progress).abs() >= MIN_NATIVE_FLOAT_LOOP_ITERATIONS as f64
}

#[cfg(feature = "cranelift")]
fn multiplicative_float_loop_is_profitable(
    current: f32,
    limit: f32,
    factor: f64,
    comparison: FloatComparison,
) -> bool {
    if factor == 1.0 {
        return true;
    }
    let moves_up = factor > 1.0;
    let exits_up = match comparison {
        FloatComparison::LessThan | FloatComparison::LessThanOrEqual => true,
        FloatComparison::GreaterThan | FloatComparison::GreaterThanOrEqual => false,
        FloatComparison::NotEqual => current < limit,
        FloatComparison::Equal => unreachable!("equal loops are handled before estimation"),
    };
    if moves_up != exits_up {
        return true;
    }
    let iterations = (f64::from(limit) / f64::from(current)).ln() / factor.ln();
    iterations.abs() >= MIN_NATIVE_FLOAT_LOOP_ITERATIONS as f64
}

#[cfg(feature = "cranelift")]
fn float_comparison_holds(left: f32, right: f32, comparison: FloatComparison) -> bool {
    match comparison {
        FloatComparison::Equal => left == right,
        FloatComparison::NotEqual => left != right,
        FloatComparison::LessThan => left < right,
        FloatComparison::LessThanOrEqual => left <= right,
        FloatComparison::GreaterThan => left > right,
        FloatComparison::GreaterThanOrEqual => left >= right,
    }
}

#[cfg(feature = "cranelift")]
fn natural_integer_loop_is_profitable(
    current: i64,
    delta: i64,
    limit: i64,
    comparison: IntegerComparison,
    max_iterations: usize,
    minimum_iterations: usize,
) -> bool {
    if max_iterations < minimum_iterations {
        return false;
    }
    let current = i128::from(current);
    let delta = i128::from(delta);
    let limit = i128::from(limit);
    let unbounded = max_iterations as i128;
    let iterations = match comparison {
        IntegerComparison::Equal => {
            if current != limit {
                0
            } else if delta == 0 {
                unbounded
            } else {
                1
            }
        }
        IntegerComparison::NotEqual => {
            let distance = limit - current;
            if distance == 0 {
                0
            } else if delta == 0 || distance.signum() != delta.signum() || distance % delta != 0 {
                unbounded
            } else {
                distance / delta
            }
        }
        IntegerComparison::LessThan => {
            let distance = limit - current;
            if distance <= 0 {
                0
            } else if delta <= 0 {
                unbounded
            } else {
                (distance + delta - 1) / delta
            }
        }
        IntegerComparison::LessThanOrEqual => {
            let distance = limit - current;
            if distance < 0 {
                0
            } else if delta <= 0 {
                unbounded
            } else {
                (distance / delta) + 1
            }
        }
        IntegerComparison::GreaterThan => {
            let distance = current - limit;
            if distance <= 0 {
                0
            } else if delta >= 0 {
                unbounded
            } else {
                let step = -delta;
                (distance + step - 1) / step
            }
        }
        IntegerComparison::GreaterThanOrEqual => {
            let distance = current - limit;
            if distance < 0 {
                0
            } else if delta >= 0 {
                unbounded
            } else {
                (distance / -delta) + 1
            }
        }
    };
    iterations >= minimum_iterations as i128
}

fn eval_unary(op: RuntimeUnaryOp, value: &Value) -> Result<Value, Value> {
    match op {
        RuntimeUnaryOp::Not => Ok(Value::bool(!truthy(value))),
        RuntimeUnaryOp::Neg => value
            .checked_neg()
            .ok_or_else(|| arithmetic_error("E_ARITH", "invalid unary arithmetic", [value])),
    }
}

fn eval_binary(op: RuntimeBinaryOp, left: &Value, right: &Value) -> Result<Value, Value> {
    match op {
        RuntimeBinaryOp::Eq => Ok(Value::bool(mica_var::language_cmp::numeric_eq(left, right))),
        RuntimeBinaryOp::Ne => Ok(Value::bool(!mica_var::language_cmp::numeric_eq(
            left, right,
        ))),
        RuntimeBinaryOp::Lt => Ok(Value::bool(
            mica_var::language_cmp::numeric_cmp(left, right) == std::cmp::Ordering::Less,
        )),
        RuntimeBinaryOp::Le => Ok(Value::bool(matches!(
            mica_var::language_cmp::numeric_cmp(left, right),
            std::cmp::Ordering::Less | std::cmp::Ordering::Equal
        ))),
        RuntimeBinaryOp::Gt => Ok(Value::bool(
            mica_var::language_cmp::numeric_cmp(left, right) == std::cmp::Ordering::Greater,
        )),
        RuntimeBinaryOp::Ge => Ok(Value::bool(matches!(
            mica_var::language_cmp::numeric_cmp(left, right),
            std::cmp::Ordering::Greater | std::cmp::Ordering::Equal
        ))),
        RuntimeBinaryOp::Add => left
            .checked_add(right)
            .ok_or_else(|| arithmetic_error("E_ARITH", "invalid addition", [left, right])),
        RuntimeBinaryOp::Sub => left
            .checked_sub(right)
            .ok_or_else(|| arithmetic_error("E_ARITH", "invalid subtraction", [left, right])),
        RuntimeBinaryOp::Mul => left
            .checked_mul(right)
            .ok_or_else(|| arithmetic_error("E_ARITH", "invalid multiplication", [left, right])),
        RuntimeBinaryOp::Div if is_zero(right) => {
            Err(arithmetic_error("E_DIV", "division by zero", [left, right]))
        }
        RuntimeBinaryOp::Div => left
            .checked_div(right)
            .ok_or_else(|| arithmetic_error("E_ARITH", "invalid division", [left, right])),
        RuntimeBinaryOp::Rem if is_zero(right) => Err(arithmetic_error(
            "E_DIV",
            "remainder by zero",
            [left, right],
        )),
        RuntimeBinaryOp::Rem => left
            .checked_rem(right)
            .ok_or_else(|| arithmetic_error("E_ARITH", "invalid remainder", [left, right])),
    }
}

fn is_zero(value: &Value) -> bool {
    value.as_int() == Some(0) || value.as_float() == Some(0.0)
}

fn arithmetic_error<const N: usize>(code: &str, message: &str, values: [&Value; N]) -> Value {
    Value::error(
        Symbol::intern(code),
        Some(message),
        Some(Value::list(values.into_iter().cloned())),
    )
}

fn index_value(collection: &Value, index: &Value) -> Result<Value, Value> {
    if let Some((start, end)) = index.with_range(|start, end| (start.clone(), end.cloned()))
        && let Some(len) = collection.list_len()
    {
        return list_range_slice(collection, len, &start, end.as_ref())
            .ok_or_else(|| index_error(collection, index));
    }
    if let Some(index) = index.as_int()
        && index >= 0
        && let Some(value) = collection.list_get(index as usize).or_else(|| {
            collection
                .with_relation(|relation| relation_row_value(relation, index as usize))
                .flatten()
        })
    {
        return Ok(value);
    }
    collection
        .map_get(index)
        .ok_or_else(|| index_error(collection, index))
}

fn list_range_slice(
    collection: &Value,
    len: usize,
    start: &Value,
    end: Option<&Value>,
) -> Option<Value> {
    let start = ordinal_index(start)?;
    let end_exclusive = match end {
        Some(end) => {
            let end = ordinal_index(end)?;
            if end < start {
                return None;
            }
            end.checked_add(1)?
        }
        None => len,
    };
    collection.list_slice(start, end_exclusive)
}

fn set_index_value(collection: &Value, index: &Value, value: Value) -> Result<Value, Value> {
    collection
        .index_set(index, value)
        .ok_or_else(|| index_error(collection, index))
}

fn index_error(collection: &Value, index: &Value) -> Value {
    Value::error(
        Symbol::intern("E_INDEX"),
        Some("collection index is missing or invalid"),
        Some(Value::list([collection.clone(), index.clone()])),
    )
}

fn error_field_value(error: &Value, field: ErrorField) -> Value {
    if let Some(code) = error.as_error_code() {
        return match field {
            ErrorField::Code => Value::error_code(code),
            ErrorField::Message | ErrorField::Value => Value::nothing(),
        };
    }
    error
        .with_error(|error| match field {
            ErrorField::Code => Value::error_code(error.code()),
            ErrorField::Message => error.message().map_or_else(Value::nothing, Value::string),
            ErrorField::Value => error.value().cloned().unwrap_or_else(Value::nothing),
        })
        .unwrap_or_else(Value::nothing)
}

fn collection_len(collection: &Value) -> Value {
    let len = collection
        .list_len()
        .or_else(|| collection.map_len())
        .or_else(|| collection.with_relation(RelationValue::len))
        .or_else(|| {
            collection.with_range(|start, end| {
                let start = start.as_int()?;
                let end = end.and_then(Value::as_int)?;
                if end < start {
                    return Some(0);
                }
                let len = end.checked_sub(start)?.checked_add(1)?;
                usize::try_from(len).ok()
            })?
        });
    let len = len.unwrap_or(0);
    i64::try_from(len)
        .ok()
        .and_then(|len| Value::int(len).ok())
        .unwrap_or_else(|| Value::int(0).expect("zero is a valid Mica integer"))
}

#[cfg(feature = "cranelift")]
fn natural_loop_collection_view(collection: &Value) -> Option<NaturalLoopCollectionView<'_>> {
    match collection.as_value_ref() {
        ValueRef::Range {
            start,
            end: Some(end),
        } => NaturalLoopCollectionView::range(start.as_int()?, end.as_int()?),
        ValueRef::List(values) => Some(NaturalLoopCollectionView::list(values)),
        ValueRef::Map(entries) => Some(NaturalLoopCollectionView::map(entries)),
        _ => None,
    }
}

fn query_relation(
    rows: Vec<Tuple>,
    outputs: &[QueryBinding],
    source_arity: usize,
) -> Result<Value, RuntimeError> {
    let mut columns = Vec::<(Symbol, usize)>::with_capacity(outputs.len());
    let mut equalities = Vec::new();
    for output in outputs {
        let position = usize::from(output.position);
        if position >= source_arity {
            return Err(RuntimeError::ProgramArtifact(
                "query binding position is out of bounds".to_owned(),
            ));
        }
        if let Some((_, first_position)) = columns.iter().find(|(name, _)| *name == output.name) {
            equalities.push((*first_position, position));
        } else {
            columns.push((output.name, position));
        }
    }

    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        if equalities
            .iter()
            .any(|(left, right)| row.values()[*left] != row.values()[*right])
        {
            continue;
        }
        result.push(Tuple::new(
            columns
                .iter()
                .map(|(_, position)| row.values()[*position].clone()),
        ));
    }

    Value::relation(columns.into_iter().map(|(name, _)| name), result)
        .map_err(|error| RuntimeError::ProgramArtifact(format!("invalid query relation: {error}")))
}

fn relation_row_value(relation: &RelationValue, index: usize) -> Option<Value> {
    let row = relation.rows().get(index)?;
    Some(Value::map(relation.heading().iter().zip(row.values()).map(
        |(column, value)| (Value::symbol(*column), value.clone()),
    )))
}

fn collection_key_at(collection: &Value, index: &Value) -> Value {
    let Some(index) = ordinal_index(index) else {
        return Value::nothing();
    };
    if collection.list_len().is_some() || collection.kind() == ValueKind::Relation {
        return i64::try_from(index)
            .ok()
            .and_then(|index| Value::int(index).ok())
            .unwrap_or_else(Value::nothing);
    }
    if collection.with_range(|_, _| ()).is_some() {
        return i64::try_from(index)
            .ok()
            .and_then(|index| Value::int(index).ok())
            .unwrap_or_else(Value::nothing);
    }
    collection
        .with_map(|entries| entries.get(index).map(|(key, _)| key.clone()))
        .flatten()
        .unwrap_or_else(Value::nothing)
}

fn collection_value_at(collection: &Value, index: &Value) -> Value {
    let Some(index) = ordinal_index(index) else {
        return Value::nothing();
    };
    collection
        .list_get(index)
        .or_else(|| {
            collection
                .with_relation(|relation| relation_row_value(relation, index))
                .flatten()
        })
        .or_else(|| {
            collection.with_range(|start, end| {
                let start = start.as_int()?;
                let end = end.and_then(Value::as_int)?;
                if end < start {
                    return None;
                }
                let offset = i64::try_from(index).ok()?;
                let value = start.checked_add(offset)?;
                if value > end {
                    return None;
                }
                Value::int(value).ok()
            })?
        })
        .or_else(|| {
            collection
                .with_map(|entries| entries.get(index).map(|(_, value)| value.clone()))
                .flatten()
        })
        .unwrap_or_else(Value::nothing)
}

fn one_value(value: &Value) -> Result<Value, Value> {
    if let Some(result) = value.with_relation(|relation| match relation.len() {
        0 => Ok(Value::nothing()),
        1 if relation.arity() == 1 => Ok(relation.rows()[0].values()[0].clone()),
        1 => Ok(relation_row_value(relation, 0).unwrap()),
        _ => Err(ambiguous_one(value)),
    }) {
        return result;
    }

    let Some(len) = value.list_len() else {
        return Ok(Value::nothing());
    };
    match len {
        0 => Ok(Value::nothing()),
        1 => {
            let row = value.list_get(0).unwrap_or_else(Value::nothing);
            if row.map_len() == Some(1) {
                return Ok(row
                    .with_map(|entries| entries[0].1.clone())
                    .unwrap_or_else(Value::nothing));
            }
            Ok(row)
        }
        _ => Err(ambiguous_one(value)),
    }
}

fn ambiguous_one(value: &Value) -> Value {
    Value::error(
        Symbol::intern("E_AMBIGUOUS"),
        Some("one expected at most one result"),
        Some(value.clone()),
    )
}

fn select_authorized_method_call(
    authority: &AuthorityContext,
    selector: Value,
    methods: Vec<ApplicableMethodCall>,
) -> Result<ApplicableMethodCall, RuntimeError> {
    let mut selected = None;
    let mut ambiguous = Vec::new();
    let mut candidate_count = 0usize;
    let mut unauthorized_count = 0usize;
    for entry in methods {
        candidate_count += 1;
        if !authority.can_invoke_method(&entry.method) {
            unauthorized_count += 1;
            continue;
        }
        if let Some(previous) = selected.replace(entry) {
            if ambiguous.is_empty() {
                ambiguous.push(previous.method);
            }
            ambiguous.push(selected.as_ref().unwrap().method.clone());
        }
    }
    if !ambiguous.is_empty() {
        return Err(RuntimeError::AmbiguousDispatch {
            selector,
            methods: ambiguous,
        });
    }
    selected.ok_or_else(|| {
        if candidate_count > 0 && unauthorized_count == candidate_count {
            tracing::warn!(
                target: "mica_vm::dispatch",
                selector = ?selector,
                candidates = candidate_count,
                "dispatch candidates were rejected by invoke authority"
            );
        }
        RuntimeError::NoApplicableMethod { selector }
    })
}

fn dynamic_dispatch_roles(value: &Value) -> Result<Vec<(Value, Value)>, RuntimeError> {
    value
        .with_map(|entries| entries.to_vec())
        .ok_or_else(|| RuntimeError::InvalidBuiltinCall {
            name: Symbol::intern("invoke"),
            message: "invoke expects roles to be a map".to_owned(),
        })
}

fn dynamic_spawn_roles(value: &Value) -> Result<Vec<(Symbol, Value)>, RuntimeError> {
    value
        .with_map(|entries| {
            entries
                .iter()
                .map(|(role, value)| {
                    role.as_symbol()
                        .map(|role| (role, value.clone()))
                        .ok_or_else(|| RuntimeError::InvalidSpawnRole(role.clone()))
                })
                .collect()
        })
        .ok_or_else(|| RuntimeError::InvalidArgumentSplice(value.clone()))?
}

fn normalize_spawn_roles(roles: &mut [(Symbol, Value)]) {
    roles.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
}

fn select_authorized_method(
    authority: &AuthorityContext,
    selector: Value,
    methods: &[Value],
) -> Result<Value, RuntimeError> {
    let mut selected = None;
    let mut ambiguous = Vec::new();
    let mut candidate_count = 0usize;
    let mut unauthorized_count = 0usize;
    for method in methods {
        candidate_count += 1;
        if !authority.can_invoke_method(method) {
            unauthorized_count += 1;
            continue;
        }
        if let Some(previous) = selected.replace(method.clone()) {
            if ambiguous.is_empty() {
                ambiguous.push(previous);
            }
            ambiguous.push(selected.as_ref().unwrap().clone());
        }
    }
    if !ambiguous.is_empty() {
        return Err(RuntimeError::AmbiguousDispatch {
            selector,
            methods: ambiguous,
        });
    }
    selected.ok_or_else(|| {
        if candidate_count > 0 && unauthorized_count == candidate_count {
            tracing::warn!(
                target: "mica_vm::dispatch",
                selector = ?selector,
                candidates = candidate_count,
                "dispatch candidates were rejected by invoke authority"
            );
        }
        RuntimeError::NoApplicableMethod { selector }
    })
}

fn ordinal_index(index: &Value) -> Option<usize> {
    let index = index.as_int()?;
    usize::try_from(index).ok()
}

fn require_read(
    authority: &AuthorityContext,
    relation: mica_relation_kernel::RelationId,
) -> Result<(), RuntimeError> {
    if authority.can_read_relation(relation) {
        Ok(())
    } else {
        Err(RuntimeError::PermissionDenied {
            operation: "read",
            target: Value::identity(relation),
        })
    }
}

fn require_write(
    authority: &AuthorityContext,
    relation: mica_relation_kernel::RelationId,
) -> Result<(), RuntimeError> {
    if authority.can_write_relation(relation) {
        Ok(())
    } else {
        Err(RuntimeError::PermissionDenied {
            operation: "write",
            target: Value::identity(relation),
        })
    }
}

fn matching_handler<'a>(catches: &'a [CatchHandler], error: &Value) -> Option<&'a CatchHandler> {
    let error_code = error.error_code_symbol();
    catches.iter().find(|catch| match &catch.code {
        Some(code) => code.error_code_symbol().is_some() && code.error_code_symbol() == error_code,
        None => true,
    })
}

fn kind_check_error(
    site: KindCheckSite,
    subject: Symbol,
    expected: ValueKind,
    actual: ValueKind,
    value: Value,
) -> Value {
    let boundary = match site {
        KindCheckSite::Binding => "binding",
        KindCheckSite::Parameter => "parameter",
        KindCheckSite::Builtin => "builtin result",
    };
    let subject = subject
        .name()
        .expect("validated kind check subjects are named");
    Value::error(
        Symbol::intern("E_TYPE"),
        Some(format!(
            "{boundary} `{subject}` requires {}, got {}",
            expected.name(),
            actual.name()
        )),
        Some(value),
    )
}

fn type_check_error(
    site: KindCheckSite,
    subject: Symbol,
    expected: &TypeContract,
    value: Value,
) -> Value {
    let boundary = match site {
        KindCheckSite::Binding => "binding",
        KindCheckSite::Parameter => "parameter",
        KindCheckSite::Builtin => "builtin result",
    };
    let subject = subject
        .name()
        .expect("validated type check subjects are named");
    Value::error(
        Symbol::intern("E_TYPE"),
        Some(format!(
            "{boundary} `{subject}` requires {expected}, got {}",
            value.kind().name()
        )),
        Some(value),
    )
}

fn value_matches_type(
    value: &Value,
    expected: &TypeContract,
    cache: &mut BTreeMap<usize, (Value, BTreeSet<TypeContract>)>,
) -> bool {
    match expected {
        TypeContract::Dynamic => true,
        TypeContract::Never => false,
        TypeContract::Kind(kind) => value.kind() == *kind,
        TypeContract::Literal(literal) => match literal {
            TypeLiteralContract::Bool(expected) => value.as_bool() == Some(*expected),
            TypeLiteralContract::Symbol(expected) => value.as_symbol() == Some(*expected),
            TypeLiteralContract::ErrorCode(expected) => value.as_error_code() == Some(*expected),
            TypeLiteralContract::Unit => value
                .with_relation(|relation| relation.arity() == 0 && relation.len() == 1)
                .unwrap_or(false),
            TypeLiteralContract::EmptyRelation => value.is_empty_relation(),
        },
        TypeContract::Union(types) => types
            .iter()
            .any(|expected| value_matches_type(value, expected, cache)),
        TypeContract::Relation(expected) => value
            .with_relation(|relation| {
                let identity = std::ptr::from_ref(relation).addr();
                if cache.get(&identity).is_some_and(|(_, checks)| {
                    checks.contains(&TypeContract::Relation(expected.clone()))
                }) {
                    return true;
                }
                let Some(first) = expected.alternatives.first() else {
                    return false;
                };
                if relation.len() < expected.minimum_rows
                    || expected
                        .maximum_rows
                        .is_some_and(|maximum| relation.len() > maximum)
                    || !relation
                        .heading()
                        .iter()
                        .copied()
                        .eq(first.columns.iter().map(|(name, _)| *name))
                {
                    return false;
                }
                let matches = relation.rows().iter().all(|row| {
                    expected.alternatives.iter().any(|alternative| {
                        row.values()
                            .iter()
                            .zip(&alternative.columns)
                            .all(|(cell, (_, contract))| value_matches_type(cell, contract, cache))
                    })
                });
                if matches {
                    cache
                        .entry(identity)
                        .or_insert_with(|| (value.clone(), BTreeSet::new()))
                        .1
                        .insert(TypeContract::Relation(expected.clone()));
                }
                matches
            })
            .unwrap_or(false),
    }
}

fn validate_builtin_result(
    name: Symbol,
    expected: Option<ValueKind>,
    value: Value,
) -> Result<Value, RuntimeError> {
    let Some(expected) = expected else {
        return Ok(value);
    };
    let actual = value.kind();
    if actual != expected {
        return Err(RuntimeError::BuiltinResultKindMismatch {
            name,
            expected,
            actual,
        });
    }
    Ok(value)
}

fn normalize_raised_error(
    error: Value,
    message: Option<Value>,
    value: Option<Value>,
) -> Result<Value, RuntimeError> {
    let message = message
        .and_then(|message| {
            if message.is_empty_relation() {
                None
            } else {
                Some(error_message_text(message))
            }
        })
        .transpose()?;
    if let Some(code) = error.as_error_code() {
        return Ok(Value::error(code, message, value));
    }
    if let Some(result) = error.with_error(|existing| {
        let message = message.or_else(|| existing.message().map(str::to_owned));
        let value = value.or_else(|| existing.value().cloned());
        Value::error(existing.code(), message, value)
    }) {
        return Ok(result);
    }
    Err(RuntimeError::InvalidRaisedValue(error))
}

fn error_message_text(value: Value) -> Result<String, RuntimeError> {
    value
        .with_str(str::to_owned)
        .ok_or(RuntimeError::InvalidErrorMessage(value))
}

#[cfg(test)]
mod structural_type_cache_tests {
    use super::*;
    use crate::{RelationRowContract, RelationTypeContract};

    #[test]
    fn repeated_checks_memoize_an_immutable_large_relation() {
        let value_column = Symbol::intern("value");
        let relation = Value::relation(
            [value_column],
            (0..4096).map(|value| Tuple::from([Value::int(value).unwrap()])),
        )
        .unwrap();
        let contract = TypeContract::Relation(RelationTypeContract {
            alternatives: vec![RelationRowContract {
                columns: vec![(value_column, TypeContract::Kind(ValueKind::Int))],
            }],
            minimum_rows: 0,
            maximum_rows: None,
        });
        let mut cache = BTreeMap::new();

        assert!(value_matches_type(&relation, &contract, &mut cache));
        assert_eq!(cache.len(), 1);
        let cached_contracts = cache.values().next().unwrap().1.len();
        assert!(value_matches_type(&relation, &contract, &mut cache));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.values().next().unwrap().1.len(), cached_contracts);
    }
}
