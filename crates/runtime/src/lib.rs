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

mod builtins;
mod embedding;
pub mod json;
pub mod metrics;
mod openai;
mod retrieval;
mod subscription;
mod task;
mod task_manager;
mod types;

#[cfg(test)]
mod vm_tests;

pub use embedding::{EmbeddingProvider, EmbeddingProviderKind};
pub use json::{JsonValueError, float_to_literal, value_from_json_text};
#[cfg(feature = "fjall")]
pub use mica_relation_kernel::FjallDurabilityMode;
pub use mica_relation_kernel::{
    ExecutionAdmission, ExecutionContext, RelationAccelerator, Tuple,
    metrics as relation_kernel_metrics,
};
pub use mica_vm::metrics as vm_metrics;
pub use mica_vm::{
    AuthorityContext, Builtin, BuiltinContext, BuiltinRegistry, BuiltinResultKind, CapabilityGrant,
    CapabilityOp, CapabilityScope, CatchHandler, Emission, ErrorField, ExternalRequest, Frame,
    Instruction, KindCheckSite, ListItem, MailboxRecvRequest, MailboxSend, MapItem, Operand,
    Program, ProgramResolver, QueryBinding, Register, RegisterVm, RelationArg, RelationRowContract,
    RelationTypeContract, RuntimeBinaryOp, RuntimeContext, RuntimeError, RuntimeUnaryOp,
    SYSTEM_ENDPOINT, SpawnRequest, SpawnTarget, SubscriptionInitialDelivery, SubscriptionOperation,
    SubscriptionRequest, SubscriptionSubject, SuspendKind, TypeContract, TypeLiteralContract,
    VmHostContext, VmHostResponse, VmState,
};
pub use task::{Task, TaskError, TaskId, TaskLimits, TaskOutcome};
pub use task_manager::{Effect, EffectLog, SuspendedTask, TaskManager, TaskManagerError};
pub use types::{
    FileinMode, FileinReport, ReadOnlySourceQueryOptions, ReadOnlySourceQueryReport,
    ReadOnlySourceQueryStatus, RunReport, SharedSourceRunner, SourceRunner, SourceTaskError,
    SubmittedTask, TaskInput, TaskRequest,
};

use types::{SourceDeclarations, SourceProjection, SourceRelationDeclaration};

use base64::{Engine, engine::general_purpose};
use mica_compiler::{
    BinaryOp, CollectionItem, CompileContext, CompileError, DiagnosticRenderOptions,
    DiagnosticSource, Expr, HirArg, HirCatch, HirCollectionItem, HirExpr, HirFunctionBody, HirItem,
    HirPlace, HirRecovery, HirRelationAtom, HostRequestFunction, Item, Literal, MethodInstallation,
    MethodKind, MethodRelations, NodeId, Span, UnaryOp, compile_semantic, format_compile_error,
    install_methods, install_rules_from_source, parse, parse_ast, parse_semantic,
};
use mica_host_protocol::{
    DomNode, diff_dom_nodes, is_supported_dom_attribute, is_supported_dom_tag,
    snapshot_payload_json, sync_payload_signature,
};
#[cfg(feature = "fjall")]
use mica_relation_kernel::FjallStateProvider;
use mica_relation_kernel::{
    ConflictPolicy, DispatchRelations, KernelError, RelationDurability, RelationId, RelationKernel,
    RelationMetadata, RelationRead, RelationSource, relation_algebra,
};
use mica_var::{Identity, PRIMITIVE_PROTOTYPES, RelationValue, Symbol, Value, ValueKind};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
#[cfg(feature = "fjall")]
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use xml::reader::{EventReader, XmlEvent};

const GENERATED_RELATION_ID_START: u64 = 0x00f0_0000_0000_0000;
const GENERATED_IDENTITY_ID_START: u64 = 0x00e0_0000_0000_0000;
const GENERATED_METHOD_ID_START: u64 = 0x00d1_0000_0000_0000;
pub const EPHEMERAL_HOST_IDENTITY_START: u64 = 0x00ff_0000_0000_0000;
pub const EPHEMERAL_HOST_IDENTITY_END: u64 = 0x00ff_8000_0000_0000;
const NAMED_IDENTITY_RELATION_ID: u64 = 0x00df_ffff_ffff_ffff;
const METHOD_SELECTOR_RELATION_ID: u64 = 0x00df_ffff_ffff_fffe;
const PARAM_RELATION_ID: u64 = 0x00df_ffff_ffff_fffd;
const DELEGATES_RELATION_ID: u64 = 0x00df_ffff_ffff_fffc;
const METHOD_PROGRAM_RELATION_ID: u64 = 0x00df_ffff_ffff_fffb;
const PROGRAM_BYTES_RELATION_ID: u64 = 0x00df_ffff_ffff_fffa;
const METHOD_SOURCE_RELATION_ID: u64 = 0x00df_ffff_ffff_fff9;
const SOURCE_OWNS_FACT_RELATION_ID: u64 = 0x00df_ffff_ffff_fff8;
const SOURCE_OWNS_RULE_RELATION_ID: u64 = 0x00df_ffff_ffff_fff7;
const SOURCE_OWNS_RELATION_RELATION_ID: u64 = 0x00df_ffff_ffff_fff6;
const ENDPOINT_RELATION_ID: u64 = 0x00df_ffff_ffff_fff5;
const ENDPOINT_ACTOR_RELATION_ID: u64 = 0x00df_ffff_ffff_fff4;
const ENDPOINT_PRINCIPAL_RELATION_ID: u64 = 0x00df_ffff_ffff_fff3;
const ENDPOINT_PROTOCOL_RELATION_ID: u64 = 0x00df_ffff_ffff_fff2;
const ENDPOINT_OPEN_RELATION_ID: u64 = 0x00df_ffff_ffff_fff1;
const RELATION_RELATION_ID: u64 = 0x00df_ffff_ffff_fff0;
const RELATION_NAME_RELATION_ID: u64 = 0x00df_ffff_ffff_ffef;
const ARITY_RELATION_ID: u64 = 0x00df_ffff_ffff_ffee;
const RULE_RELATION_ID: u64 = 0x00df_ffff_ffff_ffed;
const RULE_HEAD_RELATION_ID: u64 = 0x00df_ffff_ffff_ffec;
const RULE_SOURCE_RELATION_ID: u64 = 0x00df_ffff_ffff_ffeb;
const ACTIVE_RULE_RELATION_ID: u64 = 0x00df_ffff_ffff_ffea;
const ARGUMENT_NAME_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe9;
const CONFLICT_POLICY_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe8;
const FUNCTIONAL_KEY_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe7;
const INDEX_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe6;
const INDEX_POSITION_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe5;
const INDEX_STORAGE_KIND_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe4;
const SUBJECT_FACT_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe3;
const MENTIONED_FACT_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe2;
const EXTENSIONAL_MENTIONED_FACT_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe1;
const RELATION_DURABILITY_RELATION_ID: u64 = 0x00df_ffff_ffff_ffe0;

impl SourceRunner {
    pub fn new_empty() -> Self {
        Self::new_empty_with_embedding_provider(EmbeddingProviderKind::Deterministic)
    }

    pub fn new_empty_with_embedding_provider(kind: EmbeddingProviderKind) -> Self {
        Self::with_kernel_embedding_provider_and_host_requests(
            bootstrap_kernel(),
            embedding::embedding_provider(kind),
            default_host_request_functions(kind),
        )
    }

    #[cfg(feature = "fjall")]
    pub fn open_fjall(
        path: impl AsRef<Path>,
        durability: FjallDurabilityMode,
    ) -> Result<Self, String> {
        Self::open_fjall_with_embedding_provider(
            path,
            durability,
            EmbeddingProviderKind::Deterministic,
        )
    }

    #[cfg(feature = "fjall")]
    pub fn open_fjall_with_embedding_provider(
        path: impl AsRef<Path>,
        durability: FjallDurabilityMode,
        embedding_provider: EmbeddingProviderKind,
    ) -> Result<Self, String> {
        let provider = Arc::new(FjallStateProvider::open_with_durability(path, durability)?);
        let persisted = provider.load_state()?;
        let kernel = if persisted.version == 0 && persisted.relations.is_empty() {
            bootstrap_kernel_with_provider(provider)
        } else {
            RelationKernel::load_from_state_and_computed_relations(
                persisted,
                provider,
                retrieval::default_computed_relations(),
            )
            .map_err(|error| format!("failed to load relation kernel state: {error:?}"))?
        };
        Ok(Self::with_kernel_embedding_provider_and_host_requests(
            kernel,
            embedding::embedding_provider(embedding_provider),
            default_host_request_functions(embedding_provider),
        ))
    }

    pub fn with_kernel(kernel: RelationKernel) -> Self {
        Self::with_kernel_and_embedding_provider(kernel, embedding::default_embedding_provider())
    }

    pub fn with_kernel_and_embedding_provider(
        kernel: RelationKernel,
        embedding_provider: Arc<dyn embedding::EmbeddingProvider>,
    ) -> Self {
        Self::with_kernel_embedding_provider_and_host_requests(
            kernel,
            embedding_provider,
            Vec::new(),
        )
    }

    fn with_kernel_embedding_provider_and_host_requests(
        kernel: RelationKernel,
        embedding_provider: Arc<dyn embedding::EmbeddingProvider>,
        host_request_functions: Vec<(String, HostRequestFunction)>,
    ) -> Self {
        let next_method_identity_id = next_generated_method_identity_id(&kernel);
        let host_request_functions =
            Arc::<[(String, HostRequestFunction)]>::from(host_request_functions);
        let mut runner = Self {
            context: CompileContext::new().with_method_relations(method_relations()),
            task_manager: TaskManager::new(kernel)
                .with_builtins(Arc::new(default_builtins(embedding_provider))),
            host_request_functions,
            next_method_identity_id,
        };
        runner.refresh_context_from_catalog();
        runner
    }

    pub fn with_task_limits(mut self, limits: TaskLimits) -> Self {
        self.task_manager = self.task_manager.with_limits(limits);
        self
    }

    pub fn with_execution_context(mut self, execution_context: ExecutionContext) -> Self {
        self.task_manager = self.task_manager.with_execution_context(execution_context);
        self
    }

    pub fn into_shared(self) -> SharedSourceRunner {
        SharedSourceRunner {
            task_manager: self.task_manager.into_shared(),
            host_request_functions: self.host_request_functions,
            filein_lock: std::sync::Mutex::new(()),
        }
    }

    pub fn flush_persistence(&self) -> Result<(), SourceTaskError> {
        self.task_manager
            .kernel()
            .flush_persistence()
            .map_err(TaskManagerError::from)
            .map_err(SourceTaskError::from)
    }

    pub fn run_source(&mut self, source: &str) -> Result<RunReport, SourceTaskError> {
        let submitted = self.submit_source(Self::root_source_request(source))?;
        Ok(self.report(submitted.task_id, submitted.outcome))
    }

    fn run_source_with_stored_source(
        &mut self,
        source: &str,
        stored_source: &str,
    ) -> Result<RunReport, SourceTaskError> {
        let submitted = self.submit_root_source_chunk_with_stored_source(
            source,
            stored_source,
            SYSTEM_ENDPOINT,
            AuthorityContext::root(),
        )?;
        Ok(self.report(submitted.task_id, submitted.outcome))
    }

    pub fn root_source_request(source: impl Into<String>) -> TaskRequest {
        TaskRequest {
            principal: None,
            actor: None,
            endpoint: SYSTEM_ENDPOINT,
            authority: AuthorityContext::root(),
            input: TaskInput::Source(source.into()),
        }
    }

    pub fn source_request_as(
        &self,
        actor: Symbol,
        source: impl Into<String>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let actor_id = self.actor_identity(actor)?;
        self.source_request_as_identity(actor_id, source)
    }

    pub fn source_request_as_identity(
        &self,
        actor: Identity,
        source: impl Into<String>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let authority = authority_for_actor(self.task_manager.kernel(), actor)?;
        Ok(TaskRequest {
            principal: None,
            actor: Some(actor),
            endpoint: SYSTEM_ENDPOINT,
            authority,
            input: TaskInput::Source(source.into()),
        })
    }

    pub fn source_request_for_endpoint(
        &self,
        endpoint: Identity,
        source: impl Into<String>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        Ok(TaskRequest {
            principal: runtime_context.principal(),
            actor: runtime_context.actor(),
            endpoint,
            authority: authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?,
            input: TaskInput::Source(source.into()),
        })
    }

    pub fn run_read_only_source_query_for_endpoint(
        &mut self,
        endpoint: Identity,
        source: impl Into<String>,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        let authority =
            read_only_authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?;
        let context = self.read_only_context_for_execution(
            runtime_context.principal(),
            runtime_context.actor(),
            endpoint,
        );
        self.run_read_only_source_query(
            runtime_context,
            authority,
            &context,
            source.into(),
            options,
        )
    }

    pub fn named_identity(&self, name: Symbol) -> Result<Identity, SourceTaskError> {
        identity_named_in_kernel(self.task_manager.kernel(), name)?.ok_or_else(|| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!("unknown identity :{}", name.name().unwrap_or("<unnamed>")),
            )
        })
    }

    pub fn named_relation(&self, name: Symbol) -> Result<(Identity, u16), SourceTaskError> {
        relation_named(self.task_manager.kernel(), name).ok_or_else(|| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!("unknown relation :{}", name.name().unwrap_or("<unnamed>")),
            )
        })
    }

    pub fn contains_named_tuple(
        &self,
        name: Symbol,
        tuple: &Tuple,
    ) -> Result<bool, SourceTaskError> {
        let (relation, _) = relation_named(self.task_manager.kernel(), name).ok_or_else(|| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!("unknown relation :{}", name.name().unwrap_or("<unnamed>")),
            )
        })?;
        self.task_manager
            .kernel()
            .snapshot()
            .contains(relation, tuple)
            .map_err(TaskManagerError::from)
            .map_err(SourceTaskError::from)
    }

    pub fn submit_source(
        &mut self,
        request: TaskRequest,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let TaskRequest {
            principal,
            actor,
            endpoint,
            authority,
            input,
        } = request;
        let TaskInput::Source(source) = input else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "submit_source requires source input",
            ));
        };
        let contextual = principal.is_some() || actor.is_some();
        if contextual {
            let semantic = parse_semantic(&source);
            if let Some(item) =
                semantic.hir.items.iter().find(|item| {
                    matches!(item, HirItem::Method { .. } | HirItem::RelationRule { .. })
                })
            {
                return Err(unsupported_runner_error(
                    item_id(item),
                    semantic.span(item_id(item)).cloned(),
                    "contextual source submission cannot install methods or rules",
                ));
            }
            let context = self.context_for_execution(principal, actor, endpoint);
            let compiled = compile_semantic(semantic, &context)?;
            let runtime_context = runtime_context(principal, actor, endpoint);
            let (task_id, outcome) = self.task_manager.submit_with_context(
                Arc::new(compiled.program),
                authority,
                runtime_context,
            )?;
            self.refresh_context_from_catalog();
            return Ok(SubmittedTask { task_id, outcome });
        }

        if root_source_needs_install_chunking(&source) {
            let chunks = source_chunks(&source);
            return self.submit_root_source_chunks(chunks, endpoint, authority);
        }

        self.submit_root_source_chunk(&source, endpoint, authority)
    }

    fn submit_root_source_chunks(
        &mut self,
        chunks: Vec<String>,
        endpoint: Identity,
        authority: AuthorityContext,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let mut submitted = None;
        for chunk in chunks {
            submitted = Some(self.submit_root_source_chunk(&chunk, endpoint, authority.clone())?);
        }
        submitted.ok_or_else(|| {
            unsupported_runner_error(NodeId(0), None, "source submission contains no items")
        })
    }

    fn submit_root_source_chunk(
        &mut self,
        source: &str,
        endpoint: Identity,
        authority: AuthorityContext,
    ) -> Result<SubmittedTask, SourceTaskError> {
        self.submit_root_source_chunk_with_stored_source(source, source, endpoint, authority)
    }

    fn submit_root_source_chunk_with_stored_source(
        &mut self,
        source: &str,
        stored_source: &str,
        endpoint: Identity,
        authority: AuthorityContext,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let semantic = parse_semantic(source);
        if semantic.parse_errors.is_empty() && semantic.diagnostics.is_empty() {
            self.predeclare_source_names(&semantic)?;
        }

        if let Some(installation) = self.install_methods_from_source(source, stored_source)? {
            let value = installed_method_value(&installation);
            let (task_id, outcome) = self.task_manager.complete_immediate(value);
            self.refresh_context_from_catalog();
            return Ok(SubmittedTask { task_id, outcome });
        }

        if let Some(installation) =
            install_rules_from_source(source, &self.context, self.task_manager.kernel())?
        {
            let value = installed_rule_value(&installation.rules);
            let (task_id, outcome) = self.task_manager.complete_immediate(value);
            self.refresh_context_from_catalog();
            return Ok(SubmittedTask { task_id, outcome });
        }

        let context = self.context_for_execution(None, None, endpoint);
        let compiled = compile_semantic(semantic, &context)?;
        let runtime_context = runtime_context(None, None, endpoint);
        let (task_id, outcome) = self.task_manager.submit_with_context(
            Arc::new(compiled.program),
            authority,
            runtime_context,
        )?;
        self.refresh_context_from_catalog();
        Ok(SubmittedTask { task_id, outcome })
    }

    pub fn submit_invocation(
        &mut self,
        request: TaskRequest,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let TaskRequest {
            principal,
            actor,
            endpoint,
            authority,
            input,
        } = request;
        let TaskInput::Invocation { selector, roles } = input else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "submit_invocation requires invocation input",
            ));
        };
        let program = invocation_program(
            selector,
            invocation_roles(principal, actor, endpoint, roles),
        )
        .map_err(CompileError::from)?;
        let runtime_context = runtime_context(principal, actor, endpoint);
        let (task_id, outcome) =
            self.task_manager
                .submit_with_context(Arc::new(program), authority, runtime_context)?;
        self.refresh_context_from_catalog();
        Ok(SubmittedTask { task_id, outcome })
    }

    pub fn invocation_request_for_endpoint(
        &self,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        Ok(TaskRequest {
            principal: runtime_context.principal(),
            actor: runtime_context.actor(),
            endpoint,
            authority: authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?,
            input: TaskInput::Invocation { selector, roles },
        })
    }

    pub fn submit_invocation_for_endpoint(
        &mut self,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let request = self.invocation_request_for_endpoint(endpoint, selector, roles)?;
        self.submit_invocation(request)
    }

    pub fn resume_task(&mut self, request: TaskRequest) -> Result<TaskOutcome, SourceTaskError> {
        let TaskRequest {
            principal,
            actor,
            endpoint,
            authority,
            input,
        } = request;
        let TaskInput::Continuation { task_id, value } = input else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "resume_task requires continuation input",
            ));
        };
        let runtime_context = runtime_context(principal, actor, endpoint);
        let outcome = self
            .task_manager
            .resume_with_context(task_id, authority, value, runtime_context)
            .map_err(SourceTaskError::from)?;
        self.refresh_context_from_catalog();
        Ok(outcome)
    }

    pub fn submit_source_as(
        &mut self,
        actor: Identity,
        endpoint: Identity,
        source: impl Into<String>,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let authority = authority_for_actor(self.task_manager.kernel(), actor)?;
        self.submit_source(TaskRequest {
            principal: None,
            actor: Some(actor),
            endpoint,
            authority,
            input: TaskInput::Source(source.into()),
        })
    }

    pub fn submit_invocation_as(
        &mut self,
        actor: Identity,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let authority = authority_for_actor(self.task_manager.kernel(), actor)?;
        self.submit_invocation(TaskRequest {
            principal: None,
            actor: Some(actor),
            endpoint,
            authority,
            input: TaskInput::Invocation { selector, roles },
        })
    }

    pub fn drain_emissions(&mut self) -> Vec<Effect> {
        self.task_manager.drain_emissions()
    }

    pub fn drain_mailbox(&self, receiver: Value) -> Result<Vec<Value>, RuntimeError> {
        self.task_manager.drain_mailbox(receiver)
    }

    pub fn create_mailbox(&self) -> Result<(Value, Value), RuntimeError> {
        self.task_manager.create_mailbox()
    }

    pub fn close_mailbox(&self, receiver: &Value) -> Result<(), RuntimeError> {
        self.task_manager.close_mailbox(receiver)
    }

    pub fn mailbox_for_receiver(&self, receiver: &Value) -> Result<u64, RuntimeError> {
        self.task_manager.mailbox_for_receiver(receiver)
    }

    pub fn mailbox_for_sender(&self, sender: &Value) -> Result<u64, RuntimeError> {
        self.task_manager.mailbox_for_sender(sender)
    }

    pub fn deliver_mailbox(&self, sender: Value, value: Value) -> Result<u64, RuntimeError> {
        self.task_manager.deliver_mailbox(sender, value)
    }

    pub fn take_subscription_deliveries(&self) -> Vec<u64> {
        self.task_manager.take_subscription_deliveries()
    }

    pub fn register_subscription(
        &self,
        request: SubscriptionRequest,
        runtime_context: RuntimeContext,
        authority: &AuthorityContext,
    ) -> Result<Value, RuntimeError> {
        self.task_manager
            .register_subscription(request, runtime_context, authority)
    }

    pub fn register_subscription_for_endpoint(
        &self,
        endpoint: Identity,
        request: SubscriptionRequest,
    ) -> Result<Value, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        let authority = authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?;
        self.task_manager
            .register_subscription(request, runtime_context, &authority)
            .map_err(|error| {
                SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(error)))
            })
    }

    pub fn cancel_subscription(&self, subscription: Value) -> Result<(), RuntimeError> {
        self.task_manager.cancel_subscription(subscription)
    }

    pub fn cancel_all_subscriptions(&self) -> usize {
        self.task_manager.cancel_all_subscriptions()
    }

    pub fn cancel_task(&mut self, task_id: TaskId) -> Result<SuspendKind, SourceTaskError> {
        self.task_manager
            .cancel_task(task_id)
            .map_err(SourceTaskError::from)
    }

    pub fn cancel_all_tasks(&mut self) -> Vec<(TaskId, SuspendKind)> {
        self.task_manager.cancel_all_tasks()
    }

    pub fn forget_terminal_task(&mut self, task_id: TaskId) {
        self.task_manager.forget_terminal_task(task_id);
    }

    pub fn forget_all_terminal_tasks(&mut self) {
        self.task_manager.forget_all_terminal_tasks();
    }

    pub fn open_endpoint(
        &mut self,
        endpoint: Identity,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), SourceTaskError> {
        self.task_manager
            .open_endpoint(endpoint, actor, protocol)
            .map_err(SourceTaskError::from)
    }

    pub fn open_endpoint_with_context(
        &mut self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), SourceTaskError> {
        self.task_manager
            .open_endpoint_with_context(endpoint, principal, actor, protocol)
            .map_err(SourceTaskError::from)
    }

    pub fn open_endpoint_with_context_and_volatile_tuples_named(
        &mut self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        let tuples = volatile_tuple_relations_required(self.task_manager.kernel(), tuples)?;
        self.task_manager
            .open_endpoint_with_context_and_rows(endpoint, principal, actor, protocol, tuples)
            .map_err(SourceTaskError::from)
    }

    pub fn close_endpoint(&mut self, endpoint: Identity) -> usize {
        self.task_manager.close_endpoint(endpoint)
    }

    pub fn close_endpoint_and_retract_volatile_tuples_named(
        &mut self,
        endpoint: Identity,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        let tuples = volatile_tuple_relations_required(self.task_manager.kernel(), tuples)?;
        self.task_manager
            .close_endpoint_with_rows(endpoint, tuples)
            .map_err(SourceTaskError::from)
    }

    pub fn assert_volatile_tuples_named(
        &mut self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        assert_volatile_tuples_named_in(self.task_manager.kernel(), tuples)
    }

    pub fn retract_volatile_tuples_named(
        &mut self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        retract_volatile_tuples_named_in(self.task_manager.kernel(), tuples)
    }

    pub fn replace_volatile_tuples_named(
        &mut self,
        retract: Vec<(Symbol, Tuple)>,
        assert: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        replace_volatile_tuples_named_in(self.task_manager.kernel(), retract, assert)
    }

    pub fn route_effect_targets(&self, target: Identity) -> Vec<Identity> {
        self.task_manager.route_effect_targets(target)
    }

    pub fn drain_routed_emissions(&mut self) -> Vec<Effect> {
        self.task_manager.drain_routed_emissions()
    }

    fn endpoint_runtime_context(
        &self,
        endpoint: Identity,
    ) -> Result<RuntimeContext, SourceTaskError> {
        self.task_manager
            .endpoint_runtime_context(endpoint)
            .map_err(SourceTaskError::from)
    }

    pub fn report_outcome(&self, task_id: TaskId, outcome: TaskOutcome) -> RunReport {
        self.report(task_id, outcome)
    }

    pub fn render_source_task_error(&self, error: &SourceTaskError) -> String {
        render_source_task_error(
            error,
            &self.identity_names(),
            &self.relation_names(),
            None,
            DiagnosticRenderOptions::default(),
        )
    }

    pub fn render_task_value(&self, value: &Value) -> String {
        render_value(value, &self.identity_names(), &self.relation_names())
    }

    pub fn render_identity(&self, identity: Identity) -> String {
        render_identity(identity, &self.identity_names(), &self.relation_names())
    }

    pub fn render_source_task_error_with_source(
        &self,
        error: &SourceTaskError,
        source_name: Option<&str>,
        source: &str,
    ) -> String {
        self.render_source_task_error_with_source_options(
            error,
            source_name,
            source,
            DiagnosticRenderOptions::source_context(),
        )
    }

    pub fn render_source_task_error_with_source_options(
        &self,
        error: &SourceTaskError,
        source_name: Option<&str>,
        source: &str,
        options: DiagnosticRenderOptions,
    ) -> String {
        render_source_task_error(
            error,
            &self.identity_names(),
            &self.relation_names(),
            Some(DiagnosticSource::new(source_name, source)),
            options,
        )
    }

    fn report(&self, task_id: TaskId, outcome: TaskOutcome) -> RunReport {
        RunReport {
            task_id,
            outcome,
            identity_names: self.identity_names(),
            relation_names: self.relation_names(),
        }
    }

    fn context_for_execution(
        &self,
        principal: Option<Identity>,
        actor: Option<Identity>,
        endpoint: Identity,
    ) -> CompileContext {
        let mut context = self.context.clone();
        apply_host_request_functions(&mut context, &self.host_request_functions);
        if let Some(principal) = principal {
            context.define_identity("principal", principal);
        }
        if let Some(actor) = actor {
            context.define_identity("actor", actor);
        }
        context.define_identity("endpoint", endpoint);
        context
    }

    fn read_only_context_for_execution(
        &self,
        principal: Option<Identity>,
        actor: Option<Identity>,
        endpoint: Identity,
    ) -> CompileContext {
        let mut context = self.context.clone();
        if let Some(principal) = principal {
            context.define_identity("principal", principal);
        }
        if let Some(actor) = actor {
            context.define_identity("actor", actor);
        }
        context.define_identity("endpoint", endpoint);
        context
    }

    fn run_read_only_source_query(
        &mut self,
        runtime_context: RuntimeContext,
        authority: AuthorityContext,
        context: &CompileContext,
        source: String,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, SourceTaskError> {
        let semantic = parse_semantic(&source);
        if let Err(error) = reject_semantic_parse_or_diagnostic(&semantic) {
            return Ok(self.rejected_read_only_query_report(error, options));
        }
        if let Err(error) = validate_read_only_source_query(&semantic) {
            return Ok(self.rejected_read_only_query_report(error, options));
        }
        let compiled = match compile_semantic(semantic, context) {
            Ok(compiled) => compiled,
            Err(error) => return Ok(self.rejected_read_only_query_report(error, options)),
        };
        let (task_id, outcome) = self.task_manager.submit_with_context_and_limits(
            Arc::new(compiled.program),
            authority,
            runtime_context,
            options.task_limits(),
        )?;
        self.refresh_context_from_catalog();
        Ok(self.read_only_query_report(Some(task_id), outcome, options))
    }

    fn rejected_read_only_query_report(
        &self,
        error: CompileError,
        options: ReadOnlySourceQueryOptions,
    ) -> ReadOnlySourceQueryReport {
        let diagnostic = render_source_task_error(
            &SourceTaskError::Compile(error),
            &self.identity_names(),
            &self.relation_names(),
            None,
            DiagnosticRenderOptions::default(),
        );
        let (rendered, rendered_truncated) =
            truncate_rendered_text(diagnostic.clone(), options.max_output_chars);
        ReadOnlySourceQueryReport {
            task_id: None,
            status: ReadOnlySourceQueryStatus::Rejected,
            value: None,
            error: None,
            diagnostics: vec![diagnostic],
            rendered,
            rendered_truncated,
        }
    }

    fn read_only_query_report(
        &self,
        task_id: Option<TaskId>,
        outcome: TaskOutcome,
        options: ReadOnlySourceQueryOptions,
    ) -> ReadOnlySourceQueryReport {
        read_only_query_report(
            task_id,
            outcome,
            options,
            &self.identity_names(),
            &self.relation_names(),
        )
    }

    pub fn run_source_as(
        &mut self,
        actor: Symbol,
        source: &str,
    ) -> Result<RunReport, SourceTaskError> {
        let request = self.source_request_as(actor, source)?;
        let submitted = self.submit_source(request)?;
        Ok(self.report(submitted.task_id, submitted.outcome))
    }

    pub fn resume_as(&mut self, actor: Symbol, task_id: u64) -> Result<RunReport, SourceTaskError> {
        let actor_id = self.actor_identity(actor)?;
        let authority = authority_for_actor(self.task_manager.kernel(), actor_id)?;
        let outcome = self.resume_task(TaskRequest {
            principal: None,
            actor: Some(actor_id),
            endpoint: SYSTEM_ENDPOINT,
            authority,
            input: TaskInput::Continuation {
                task_id,
                value: Value::nothing(),
            },
        })?;
        Ok(self.report(task_id, outcome))
    }

    pub fn run_filein(&mut self, source: &str) -> Result<Vec<RunReport>, SourceTaskError> {
        let source = expand_filein_grant_blocks(source)?;
        let mut reports = Vec::new();
        for chunk in source_chunks_with_offsets(&source) {
            reports.push(
                self.run_source(&chunk.text)
                    .map_err(|error| shift_source_task_error(error, chunk.start))?,
            );
        }
        Ok(reports)
    }

    pub fn run_filein_with_include_loader(
        &mut self,
        source: &str,
        mut load_include: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Vec<RunReport>, SourceTaskError> {
        let source = expand_filein_grant_blocks(source)?;
        let mut reports = Vec::new();
        for chunk in source_chunks_with_offsets(&source) {
            let expanded = expand_filein_text_includes(&chunk.text, &mut load_include)?;
            reports.push(
                self.run_source_with_stored_source(&expanded, &chunk.text)
                    .map_err(|error| shift_source_task_error(error, chunk.start))?,
            );
        }
        Ok(reports)
    }

    pub fn check_filein_with_include_loader(
        &mut self,
        source: &str,
        mut load_include: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Vec<RunReport>, SourceTaskError> {
        let source = expand_filein_grant_blocks(source)?;
        let mut reports = Vec::new();
        let mut errors = Vec::new();
        for chunk in source_chunks_with_offsets(&source) {
            let expanded = expand_filein_text_includes(&chunk.text, &mut load_include)?;
            match self.run_source_with_stored_source(&expanded, &chunk.text) {
                Ok(report) => reports.push(report),
                Err(SourceTaskError::Compile(error)) => {
                    errors.push(shift_compile_error(error, chunk.start));
                }
                Err(error) => return Err(error),
            }
        }
        return_compile_errors(errors)?;
        Ok(reports)
    }

    pub fn run_filein_with_unit(
        &mut self,
        unit: Symbol,
        source: &str,
        mode: FileinMode,
    ) -> Result<FileinReport, SourceTaskError> {
        self.run_filein_with_unit_inner(
            unit,
            source,
            mode,
            None::<fn(&str) -> Result<String, String>>,
        )
    }

    pub fn run_filein_with_unit_and_include_loader(
        &mut self,
        unit: Symbol,
        source: &str,
        mode: FileinMode,
        load_include: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<FileinReport, SourceTaskError> {
        self.run_filein_with_unit_inner(unit, source, mode, Some(load_include))
    }

    pub fn fileout_unit(&self, unit: Symbol) -> Result<String, SourceTaskError> {
        Ok(fileout_unit_source(self.task_manager.kernel(), unit).map_err(CompileError::from)?)
    }

    fn run_filein_with_unit_inner(
        &mut self,
        unit: Symbol,
        source: &str,
        mode: FileinMode,
        include_loader: Option<impl FnMut(&str) -> Result<String, String>>,
    ) -> Result<FileinReport, SourceTaskError> {
        if mode != FileinMode::Replace {
            return self.run_filein_with_unit_direct(unit, source, mode, include_loader);
        }

        let expected_version = self.task_manager.kernel().snapshot().version();
        let staged_kernel = self.task_manager.kernel().fork_in_memory();
        let mut staged = Self {
            context: self.context.clone(),
            task_manager: self.task_manager.fork_with_kernel(staged_kernel),
            host_request_functions: self.host_request_functions.clone(),
            next_method_identity_id: self.next_method_identity_id,
        };
        let mut report = staged.run_filein_with_unit_direct(unit, source, mode, include_loader)?;
        ensure_filein_replacement_completed(&report.reports)?;
        self.task_manager
            .kernel()
            .commit_staged_snapshot(expected_version, staged.task_manager.kernel().snapshot())
            .map_err(CompileError::from)?;

        self.next_method_identity_id = staged.next_method_identity_id;
        self.refresh_context_from_catalog();
        if let Err(error) = self.task_manager.dispatch_subscriptions() {
            tracing::error!(
                ?error,
                "failed to dispatch atomic filein subscription batches"
            );
        }
        for task_report in &mut report.reports {
            task_report.task_id = self
                .task_manager
                .record_completed_outcome(task_report.outcome.clone());
        }
        Ok(report)
    }

    fn run_filein_with_unit_direct(
        &mut self,
        unit: Symbol,
        source: &str,
        mode: FileinMode,
        include_loader: Option<impl FnMut(&str) -> Result<String, String>>,
    ) -> Result<FileinReport, SourceTaskError> {
        if mode == FileinMode::Replace {
            self.retract_source_unit(unit)?;
        }

        let declarations = collect_source_declarations(source)?;
        let before = self.source_projection()?;
        let reports = if let Some(load_include) = include_loader {
            self.run_filein_with_include_loader(source, load_include)?
        } else {
            self.run_filein(source)?
        };
        let after = self.source_projection()?;

        let owned_facts = after
            .facts
            .difference(&before.facts)
            .filter(|(relation, _)| is_ownable_fact_relation(*relation))
            .filter(|(relation, _)| *relation != named_identity_relation())
            .cloned()
            .collect::<BTreeSet<_>>();
        let owned_rules = after
            .rules
            .difference(&before.rules)
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut owned_relations = after
            .relations
            .keys()
            .filter(|relation| !before.relations.contains_key(relation))
            .copied()
            .collect::<BTreeSet<_>>();

        for declaration in declarations.relations {
            if let Some((relation, existing_arity)) = relation_named(
                self.task_manager.kernel(),
                Symbol::intern(&declaration.name),
            ) && existing_arity == declaration.arity
            {
                owned_relations.insert(relation);
            }
        }

        let mut tx = self.task_manager.kernel().begin();
        for identity_name in declarations.identities {
            if let Some(identity) = identity_named_in_tx(&tx, Symbol::intern(&identity_name))
                .map_err(CompileError::from)?
            {
                tx.assert(
                    source_owns_fact_relation(),
                    ownership_fact_tuple(
                        unit,
                        named_identity_relation(),
                        Tuple::from([
                            Value::symbol(Symbol::intern(&identity_name)),
                            Value::identity(identity),
                        ]),
                    ),
                )
                .map_err(CompileError::from)?;
            }
        }
        for relation in &owned_relations {
            tx.assert(
                source_owns_relation_relation(),
                Tuple::from([Value::symbol(unit), Value::identity(*relation)]),
            )
            .map_err(CompileError::from)?;
        }
        for (relation, tuple) in &owned_facts {
            tx.assert(
                source_owns_fact_relation(),
                ownership_fact_tuple(unit, *relation, tuple.clone()),
            )
            .map_err(CompileError::from)?;
        }
        for rule in &owned_rules {
            tx.assert(
                source_owns_rule_relation(),
                Tuple::from([Value::symbol(unit), Value::identity(*rule)]),
            )
            .map_err(CompileError::from)?;
        }
        tx.commit().map_err(CompileError::from)?;
        self.refresh_context_from_catalog();

        Ok(FileinReport {
            reports,
            owned_facts: owned_facts.len(),
            owned_rules: owned_rules.len(),
            owned_relations: owned_relations.len(),
        })
    }

    fn actor_identity(&self, actor: Symbol) -> Result<Identity, SourceTaskError> {
        self.named_identity(actor).map_err(|_| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!(
                    "unknown actor identity :{}",
                    actor.name().unwrap_or("<unnamed>")
                ),
            )
        })
    }

    fn install_methods_from_source(
        &mut self,
        source: &str,
        stored_source: &str,
    ) -> Result<Option<MethodInstallation>, SourceTaskError> {
        let mut semantic = parse_semantic(source);
        if !semantic
            .hir
            .items
            .iter()
            .any(|item| matches!(item, HirItem::Method { .. }))
        {
            return Ok(None);
        }

        if !semantic.parse_errors.is_empty() {
            return Err(CompileError::ParseErrors {
                errors: semantic.parse_errors.clone(),
            }
            .into());
        }
        if let Some(diagnostic) = semantic.diagnostics.first() {
            return Err(CompileError::SemanticDiagnostic {
                diagnostic: diagnostic.clone(),
            }
            .into());
        }

        if let Some(item) = semantic
            .hir
            .items
            .iter()
            .find(|item| !matches!(item, HirItem::Method { .. }))
        {
            return Err(CompileError::Unsupported {
                node: item_id(item),
                span: semantic.span(item_id(item)).cloned(),
                message: "method definitions cannot be mixed with executable task code yet"
                    .to_owned(),
            }
            .into());
        }

        let mut install_context = self.context.clone();
        let mut next_method_identity_id = self.next_method_identity_id;
        assign_generated_verb_identities(&mut semantic, next_method_identity_id)?;
        let mut install_tx = self.task_manager.kernel().begin();
        for item in &semantic.hir.items {
            let HirItem::Method { identity, .. } = item else {
                continue;
            };
            let identity_name = identity.as_ref().ok_or_else(|| CompileError::Unsupported {
                node: item_id(item),
                span: semantic.span(item_id(item)).cloned(),
                message: "method installation requires an explicit identity".to_owned(),
            })?;
            let method_id = ensure_named_identity(
                &mut install_tx,
                identity_name,
                &mut next_method_identity_id,
            )?;
            let program_name = format!("{identity_name}_program");
            let program_id = ensure_named_identity(
                &mut install_tx,
                &program_name,
                &mut next_method_identity_id,
            )?;
            install_context.define_identity(identity_name, method_id);
            install_context.define_identity(&program_name, program_id);
            install_context.define_program_identity(identity_name, program_id);
        }

        let installation = install_methods(semantic, &install_context, &mut install_tx)?;
        for method in &installation.methods {
            install_tx
                .assert(
                    method_source_relation(),
                    Tuple::from([method.method.clone(), Value::string(stored_source)]),
                )
                .map_err(CompileError::from)?;
        }
        install_tx.commit().map_err(CompileError::from)?;
        self.context = install_context;
        self.next_method_identity_id = next_method_identity_id;
        Ok(Some(installation))
    }

    fn predeclare_source_names(
        &mut self,
        semantic: &mica_compiler::SemanticProgram,
    ) -> Result<(), SourceTaskError> {
        predeclare_source_names_in_kernel(self.task_manager.kernel(), semantic)?;
        self.refresh_context_from_catalog();
        Ok(())
    }

    fn refresh_context_from_catalog(&mut self) {
        self.context =
            compile_context_from_catalog(self.task_manager.kernel(), self.task_manager.builtins());
        apply_host_request_functions(&mut self.context, &self.host_request_functions);
    }

    fn retract_source_unit(&mut self, unit: Symbol) -> Result<(), SourceTaskError> {
        let snapshot = self.task_manager.kernel().snapshot();
        let owned_rules = snapshot
            .scan(
                source_owns_rule_relation(),
                &[Some(Value::symbol(unit)), None],
            )
            .map_err(CompileError::from)?
            .into_iter()
            .filter_map(|tuple| tuple.values().get(1).and_then(Value::as_identity))
            .collect::<Vec<_>>();
        for rule in owned_rules {
            self.task_manager
                .kernel()
                .disable_rule(rule)
                .map_err(CompileError::from)?;
        }

        let mut tx = self.task_manager.kernel().begin();
        for ownership in snapshot
            .scan(
                source_owns_fact_relation(),
                &[Some(Value::symbol(unit)), None, None],
            )
            .map_err(CompileError::from)?
        {
            if let Some((relation, tuple)) = owned_fact_tuple(&ownership)
                && relation != named_identity_relation()
            {
                tx.retract(relation, tuple).map_err(CompileError::from)?;
            }
            tx.retract(source_owns_fact_relation(), ownership)
                .map_err(CompileError::from)?;
        }
        for ownership in snapshot
            .scan(
                source_owns_rule_relation(),
                &[Some(Value::symbol(unit)), None],
            )
            .map_err(CompileError::from)?
        {
            tx.retract(source_owns_rule_relation(), ownership)
                .map_err(CompileError::from)?;
        }
        for ownership in snapshot
            .scan(
                source_owns_relation_relation(),
                &[Some(Value::symbol(unit)), None],
            )
            .map_err(CompileError::from)?
        {
            tx.retract(source_owns_relation_relation(), ownership)
                .map_err(CompileError::from)?;
        }
        tx.commit().map_err(CompileError::from)?;
        self.refresh_context_from_catalog();
        Ok(())
    }

    fn source_projection(&self) -> Result<SourceProjection, SourceTaskError> {
        let snapshot = self.task_manager.kernel().snapshot();
        let facts = snapshot
            .extensional_facts()
            .map_err(CompileError::from)?
            .into_iter()
            .collect();
        let rules = snapshot
            .rules()
            .iter()
            .filter(|rule| rule.active())
            .map(|rule| rule.id())
            .collect();
        let relations = snapshot
            .relation_metadata()
            .map(|metadata| (metadata.id(), metadata.clone()))
            .collect();
        Ok(SourceProjection {
            facts,
            rules,
            relations,
        })
    }

    fn identity_names(&self) -> BTreeMap<Identity, String> {
        let snapshot = self.task_manager.kernel().snapshot();
        snapshot
            .scan(named_identity_relation(), &[None, None])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tuple| {
                let [name, identity] = tuple.values() else {
                    return None;
                };
                let name = name.as_symbol()?.name()?.to_owned();
                let identity = identity.as_identity()?;
                Some((identity, name))
            })
            .chain(
                snapshot
                    .rules()
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| (rule.id(), format!("rule{}", index + 1))),
            )
            .collect()
    }

    fn relation_names(&self) -> BTreeMap<Identity, String> {
        let snapshot = self.task_manager.kernel().snapshot();
        snapshot
            .relation_metadata()
            .filter_map(|metadata| Some((metadata.id(), metadata.name().name()?.to_owned())))
            .collect()
    }
}

impl SharedSourceRunner {
    pub fn flush_persistence(&self) -> Result<(), SourceTaskError> {
        self.task_manager
            .kernel()
            .flush_persistence()
            .map_err(TaskManagerError::from)
            .map_err(SourceTaskError::from)
    }

    pub fn check_filein(&self, source: &str) -> Result<Vec<RunReport>, SourceTaskError> {
        self.check_filein_with_include_loader(source, |_| {
            Err("filein includes require an include loader".to_owned())
        })
    }

    pub fn check_filein_with_include_loader(
        &self,
        source: &str,
        load_include: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Vec<RunReport>, SourceTaskError> {
        let _filein = self.filein_lock.lock().unwrap();
        let mut staged = self.staged_filein_runner();
        staged.check_filein_with_include_loader(source, load_include)
    }

    pub fn run_filein_with_unit(
        &self,
        unit: Symbol,
        source: &str,
        mode: FileinMode,
    ) -> Result<FileinReport, SourceTaskError> {
        self.run_filein_with_unit_and_include_loader(unit, source, mode, |_| {
            Err("filein includes require an include loader".to_owned())
        })
    }

    pub fn run_filein_with_unit_and_include_loader(
        &self,
        unit: Symbol,
        source: &str,
        mode: FileinMode,
        load_include: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<FileinReport, SourceTaskError> {
        let _filein = self.filein_lock.lock().unwrap();
        let expected_version = self.task_manager.kernel().snapshot().version();
        let mut staged = self.staged_filein_runner();
        let mut report =
            staged.run_filein_with_unit_and_include_loader(unit, source, mode, load_include)?;
        ensure_filein_replacement_completed(&report.reports)?;
        self.task_manager
            .kernel()
            .commit_staged_snapshot(expected_version, staged.task_manager.kernel().snapshot())
            .map_err(CompileError::from)?;
        if let Err(error) = self.task_manager.dispatch_subscriptions() {
            tracing::error!(
                ?error,
                "failed to dispatch atomic filein subscription batches"
            );
        }
        for task_report in &mut report.reports {
            task_report.task_id = self
                .task_manager
                .record_completed_outcome(task_report.outcome.clone());
        }
        Ok(report)
    }

    pub fn fileout_unit(&self, unit: Symbol) -> Result<String, SourceTaskError> {
        Ok(fileout_unit_source(self.task_manager.kernel(), unit).map_err(CompileError::from)?)
    }

    fn staged_filein_runner(&self) -> SourceRunner {
        let kernel = self.task_manager.kernel().fork_in_memory();
        let mut context = compile_context_from_catalog(&kernel, self.task_manager.builtins());
        apply_host_request_functions(&mut context, &self.host_request_functions);
        let next_method_identity_id = next_generated_method_identity_id(&kernel);
        SourceRunner {
            context,
            task_manager: self.task_manager.fork_with_kernel(kernel),
            host_request_functions: self.host_request_functions.clone(),
            next_method_identity_id,
        }
    }

    pub fn named_identity(&self, name: Symbol) -> Result<Identity, SourceTaskError> {
        identity_named_in_kernel(self.task_manager.kernel(), name)?.ok_or_else(|| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!("unknown identity :{}", name.name().unwrap_or("<unnamed>")),
            )
        })
    }

    pub fn named_relation(&self, name: Symbol) -> Result<(Identity, u16), SourceTaskError> {
        relation_named(self.task_manager.kernel(), name).ok_or_else(|| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!("unknown relation :{}", name.name().unwrap_or("<unnamed>")),
            )
        })
    }

    pub fn contains_named_tuple(
        &self,
        name: Symbol,
        tuple: &Tuple,
    ) -> Result<bool, SourceTaskError> {
        let (relation, _) = relation_named(self.task_manager.kernel(), name).ok_or_else(|| {
            unsupported_runner_error(
                NodeId(0),
                None,
                format!("unknown relation :{}", name.name().unwrap_or("<unnamed>")),
            )
        })?;
        self.task_manager
            .kernel()
            .snapshot()
            .contains(relation, tuple)
            .map_err(TaskManagerError::from)
            .map_err(SourceTaskError::from)
    }

    pub fn source_request_for_endpoint(
        &self,
        endpoint: Identity,
        source: impl Into<String>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        Ok(TaskRequest {
            principal: runtime_context.principal(),
            actor: runtime_context.actor(),
            endpoint,
            authority: authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?,
            input: TaskInput::Source(source.into()),
        })
    }

    pub fn run_read_only_source_query_for_endpoint(
        &self,
        endpoint: Identity,
        source: impl Into<String>,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        let authority =
            read_only_authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?;
        let context = self.read_only_context_for_execution(
            runtime_context.principal(),
            runtime_context.actor(),
            endpoint,
        );
        self.run_read_only_source_query(
            runtime_context,
            authority,
            &context,
            source.into(),
            options,
        )
    }

    pub fn source_request_as(
        &self,
        actor: Symbol,
        source: impl Into<String>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let actor_id =
            identity_named_in_kernel(self.task_manager.kernel(), actor)?.ok_or_else(|| {
                unsupported_runner_error(
                    NodeId(0),
                    None,
                    format!("unknown actor :{}", actor.name().unwrap_or("<unnamed>")),
                )
            })?;
        self.source_request_as_identity(actor_id, source)
    }

    pub fn source_request_as_identity(
        &self,
        actor: Identity,
        source: impl Into<String>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let authority = authority_for_actor(self.task_manager.kernel(), actor)?;
        Ok(TaskRequest {
            principal: None,
            actor: Some(actor),
            endpoint: SYSTEM_ENDPOINT,
            authority,
            input: TaskInput::Source(source.into()),
        })
    }

    pub fn submit_source(&self, request: TaskRequest) -> Result<SubmittedTask, SourceTaskError> {
        let TaskRequest {
            principal,
            actor,
            endpoint,
            authority,
            input,
        } = request;
        let TaskInput::Source(source) = input else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "submit_source requires source input",
            ));
        };
        let contextual = principal.is_some() || actor.is_some() || endpoint != SYSTEM_ENDPOINT;
        if !contextual {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "shared source submission requires endpoint, actor, or principal context",
            ));
        }
        let semantic = parse_semantic(&source);
        if let Some(item) = semantic
            .hir
            .items
            .iter()
            .find(|item| matches!(item, HirItem::Method { .. } | HirItem::RelationRule { .. }))
        {
            return Err(unsupported_runner_error(
                item_id(item),
                semantic.span(item_id(item)).cloned(),
                "contextual source submission cannot install methods or rules",
            ));
        }
        if authority.can_grant()
            && semantic.parse_errors.is_empty()
            && semantic.diagnostics.is_empty()
        {
            predeclare_source_names_in_kernel(self.task_manager.kernel(), &semantic)?;
        }
        let context = self.context_for_execution(principal, actor, endpoint);
        let compiled = compile_semantic(semantic, &context)?;
        let runtime_context = runtime_context(principal, actor, endpoint);
        let (task_id, outcome) = self.task_manager.submit_with_context(
            Arc::new(compiled.program),
            authority,
            runtime_context,
        )?;
        Ok(SubmittedTask { task_id, outcome })
    }

    pub fn submit_root_source(
        &self,
        source: impl Into<String>,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let source = source.into();
        let semantic = parse_semantic(&source);
        if AuthorityContext::root().can_grant()
            && semantic.parse_errors.is_empty()
            && semantic.diagnostics.is_empty()
        {
            predeclare_source_names_in_kernel(self.task_manager.kernel(), &semantic)?;
        }
        let context = self.context_for_execution(None, None, SYSTEM_ENDPOINT);
        let compiled = compile_semantic(semantic, &context)?;
        let (task_id, outcome) = self.task_manager.submit_with_context(
            Arc::new(compiled.program),
            AuthorityContext::root(),
            runtime_context(None, None, SYSTEM_ENDPOINT),
        )?;
        Ok(SubmittedTask { task_id, outcome })
    }

    pub fn submit_source_report(
        &self,
        endpoint: Identity,
        actor: Option<Symbol>,
        source: String,
    ) -> Result<RunReport, SourceTaskError> {
        let request = match actor {
            Some(actor) => self.source_request_as(actor, source)?,
            None => self.source_request_for_endpoint(endpoint, source)?,
        };
        let submitted = self.submit_source(request)?;
        Ok(self.report(submitted.task_id, submitted.outcome))
    }

    pub fn submit_invocation(
        &self,
        request: TaskRequest,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let TaskRequest {
            principal,
            actor,
            endpoint,
            authority,
            input,
        } = request;
        let TaskInput::Invocation { selector, roles } = input else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "submit_invocation requires invocation input",
            ));
        };
        let program = invocation_program(
            selector,
            invocation_roles(principal, actor, endpoint, roles),
        )
        .map_err(CompileError::from)?;
        let runtime_context = runtime_context(principal, actor, endpoint);
        let (task_id, outcome) =
            self.task_manager
                .submit_with_context(Arc::new(program), authority, runtime_context)?;
        Ok(SubmittedTask { task_id, outcome })
    }

    pub fn invocation_request_for_endpoint(
        &self,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<TaskRequest, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        Ok(TaskRequest {
            principal: runtime_context.principal(),
            actor: runtime_context.actor(),
            endpoint,
            authority: authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?,
            input: TaskInput::Invocation { selector, roles },
        })
    }

    pub fn submit_invocation_for_endpoint(
        &self,
        endpoint: Identity,
        selector: Symbol,
        roles: Vec<(Symbol, Value)>,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let request = self.invocation_request_for_endpoint(endpoint, selector, roles)?;
        self.submit_invocation(request)
    }

    pub fn submit_spawn(
        &self,
        principal: Option<Identity>,
        actor: Option<Identity>,
        endpoint: Identity,
        parent_authority: AuthorityContext,
        spawn: SpawnRequest,
    ) -> Result<SubmittedTask, SourceTaskError> {
        let SpawnRequest {
            selector,
            target,
            delay_millis,
        } = spawn;
        let runtime_context = runtime_context(principal, actor, endpoint);
        let authority = match actor {
            Some(actor) => authority_for_actor(self.task_manager.kernel(), actor)?,
            None => parent_authority,
        };
        let program =
            spawn_invocation_program(selector, target, principal, actor, endpoint, delay_millis)
                .map_err(CompileError::from)?;
        let (task_id, outcome) =
            self.task_manager
                .submit_with_context(Arc::new(program), authority, runtime_context)?;
        Ok(SubmittedTask { task_id, outcome })
    }

    pub fn resume_task(&self, request: TaskRequest) -> Result<TaskOutcome, SourceTaskError> {
        let TaskRequest {
            principal,
            actor,
            endpoint,
            authority,
            input,
        } = request;
        let TaskInput::Continuation { task_id, value } = input else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "resume_task requires continuation input",
            ));
        };
        let runtime_context = runtime_context(principal, actor, endpoint);
        self.task_manager
            .resume_with_context(task_id, authority, value, runtime_context)
            .map_err(SourceTaskError::from)
    }

    pub fn open_endpoint(
        &self,
        endpoint: Identity,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), SourceTaskError> {
        self.task_manager
            .open_endpoint(endpoint, actor, protocol)
            .map_err(SourceTaskError::from)
    }

    pub fn open_endpoint_with_context(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
    ) -> Result<(), SourceTaskError> {
        self.task_manager
            .open_endpoint_with_context(endpoint, principal, actor, protocol)
            .map_err(SourceTaskError::from)
    }

    pub fn open_endpoint_with_context_and_volatile_tuples_named(
        &self,
        endpoint: Identity,
        principal: Option<Identity>,
        actor: Option<Identity>,
        protocol: Symbol,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        let tuples = volatile_tuple_relations_required(self.task_manager.kernel(), tuples)?;
        self.task_manager
            .open_endpoint_with_context_and_rows(endpoint, principal, actor, protocol, tuples)
            .map_err(SourceTaskError::from)
    }

    pub fn close_endpoint(&self, endpoint: Identity) -> usize {
        self.task_manager.close_endpoint(endpoint)
    }

    pub fn close_endpoint_and_retract_volatile_tuples_named(
        &self,
        endpoint: Identity,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        let tuples = volatile_tuple_relations_required(self.task_manager.kernel(), tuples)?;
        self.task_manager
            .close_endpoint_with_rows(endpoint, tuples)
            .map_err(SourceTaskError::from)
    }

    pub fn assert_volatile_tuples_named(
        &self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        assert_volatile_tuples_named_in(self.task_manager.kernel(), tuples)
    }

    pub fn retract_volatile_tuples_named(
        &self,
        tuples: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        retract_volatile_tuples_named_in(self.task_manager.kernel(), tuples)
    }

    pub fn replace_volatile_tuples_named(
        &self,
        retract: Vec<(Symbol, Tuple)>,
        assert: Vec<(Symbol, Tuple)>,
    ) -> Result<usize, SourceTaskError> {
        replace_volatile_tuples_named_in(self.task_manager.kernel(), retract, assert)
    }

    pub fn drain_emissions(&self) -> Vec<Effect> {
        self.task_manager.drain_emissions()
    }

    pub fn drain_mailbox(&self, receiver: Value) -> Result<Vec<Value>, RuntimeError> {
        self.task_manager.drain_mailbox(receiver)
    }

    pub fn create_mailbox(&self) -> Result<(Value, Value), RuntimeError> {
        self.task_manager.create_mailbox()
    }

    pub fn close_mailbox(&self, receiver: &Value) -> Result<(), RuntimeError> {
        self.task_manager.close_mailbox(receiver)
    }

    pub fn mailbox_for_receiver(&self, receiver: &Value) -> Result<u64, RuntimeError> {
        self.task_manager.mailbox_for_receiver(receiver)
    }

    pub fn mailbox_for_sender(&self, sender: &Value) -> Result<u64, RuntimeError> {
        self.task_manager.mailbox_for_sender(sender)
    }

    pub fn deliver_mailbox(&self, sender: Value, value: Value) -> Result<u64, RuntimeError> {
        self.task_manager.deliver_mailbox(sender, value)
    }

    pub fn take_subscription_deliveries(&self) -> Vec<u64> {
        self.task_manager.take_subscription_deliveries()
    }

    pub fn register_subscription(
        &self,
        request: SubscriptionRequest,
        runtime_context: RuntimeContext,
        authority: &AuthorityContext,
    ) -> Result<Value, RuntimeError> {
        self.task_manager
            .register_subscription(request, runtime_context, authority)
    }

    pub fn register_subscription_for_endpoint(
        &self,
        endpoint: Identity,
        request: SubscriptionRequest,
    ) -> Result<Value, SourceTaskError> {
        let runtime_context = self.endpoint_runtime_context(endpoint)?;
        let authority = authority_for_runtime_context(self.task_manager.kernel(), runtime_context)?;
        self.task_manager
            .register_subscription(request, runtime_context, &authority)
            .map_err(|error| {
                SourceTaskError::TaskManager(TaskManagerError::Task(TaskError::Runtime(error)))
            })
    }

    pub fn cancel_subscription(&self, subscription: Value) -> Result<(), RuntimeError> {
        self.task_manager.cancel_subscription(subscription)
    }

    pub fn cancel_all_subscriptions(&self) -> usize {
        self.task_manager.cancel_all_subscriptions()
    }

    pub fn cancel_task(&self, task_id: TaskId) -> Result<SuspendKind, SourceTaskError> {
        self.task_manager
            .cancel_task(task_id)
            .map_err(SourceTaskError::from)
    }

    pub fn cancel_all_tasks(&self) -> Vec<(TaskId, SuspendKind)> {
        self.task_manager.cancel_all_tasks()
    }

    pub fn forget_terminal_task(&self, task_id: TaskId) {
        self.task_manager.forget_terminal_task(task_id);
    }

    pub fn drain_routed_emissions(&self) -> Vec<Effect> {
        self.task_manager.drain_routed_emissions()
    }

    fn endpoint_runtime_context(
        &self,
        endpoint: Identity,
    ) -> Result<RuntimeContext, SourceTaskError> {
        self.task_manager
            .endpoint_runtime_context(endpoint)
            .map_err(SourceTaskError::from)
    }

    pub fn report_outcome(&self, task_id: TaskId, outcome: TaskOutcome) -> RunReport {
        self.report(task_id, outcome)
    }

    pub fn render_source_task_error(&self, error: &SourceTaskError) -> String {
        render_source_task_error(
            error,
            &self.identity_names(),
            &self.relation_names(),
            None,
            DiagnosticRenderOptions::default(),
        )
    }

    pub fn render_task_value(&self, value: &Value) -> String {
        render_value(value, &self.identity_names(), &self.relation_names())
    }

    pub fn render_identity(&self, identity: Identity) -> String {
        render_identity(identity, &self.identity_names(), &self.relation_names())
    }

    pub fn render_source_task_error_with_source(
        &self,
        error: &SourceTaskError,
        source_name: Option<&str>,
        source: &str,
    ) -> String {
        self.render_source_task_error_with_source_options(
            error,
            source_name,
            source,
            DiagnosticRenderOptions::source_context(),
        )
    }

    pub fn render_source_task_error_with_source_options(
        &self,
        error: &SourceTaskError,
        source_name: Option<&str>,
        source: &str,
        options: DiagnosticRenderOptions,
    ) -> String {
        render_source_task_error(
            error,
            &self.identity_names(),
            &self.relation_names(),
            Some(DiagnosticSource::new(source_name, source)),
            options,
        )
    }

    pub fn completed_len(&self) -> usize {
        self.task_manager.completed_len()
    }

    pub fn cancelled_len(&self) -> usize {
        self.task_manager.cancelled_len()
    }

    pub fn suspended_len(&self) -> usize {
        self.task_manager.suspended_len()
    }

    fn report(&self, task_id: TaskId, outcome: TaskOutcome) -> RunReport {
        RunReport {
            task_id,
            outcome,
            identity_names: self.identity_names(),
            relation_names: self.relation_names(),
        }
    }

    fn context_for_execution(
        &self,
        principal: Option<Identity>,
        actor: Option<Identity>,
        endpoint: Identity,
    ) -> CompileContext {
        let mut context =
            compile_context_from_catalog(self.task_manager.kernel(), self.task_manager.builtins());
        apply_host_request_functions(&mut context, &self.host_request_functions);
        if let Some(principal) = principal {
            context.define_identity("principal", principal);
        }
        if let Some(actor) = actor {
            context.define_identity("actor", actor);
        }
        context.define_identity("endpoint", endpoint);
        context
    }

    fn read_only_context_for_execution(
        &self,
        principal: Option<Identity>,
        actor: Option<Identity>,
        endpoint: Identity,
    ) -> CompileContext {
        let mut context =
            compile_context_from_catalog(self.task_manager.kernel(), self.task_manager.builtins());
        if let Some(principal) = principal {
            context.define_identity("principal", principal);
        }
        if let Some(actor) = actor {
            context.define_identity("actor", actor);
        }
        context.define_identity("endpoint", endpoint);
        context
    }

    fn run_read_only_source_query(
        &self,
        runtime_context: RuntimeContext,
        authority: AuthorityContext,
        context: &CompileContext,
        source: String,
        options: ReadOnlySourceQueryOptions,
    ) -> Result<ReadOnlySourceQueryReport, SourceTaskError> {
        let semantic = parse_semantic(&source);
        if let Err(error) = reject_semantic_parse_or_diagnostic(&semantic) {
            return Ok(self.rejected_read_only_query_report(error, options));
        }
        if let Err(error) = validate_read_only_source_query(&semantic) {
            return Ok(self.rejected_read_only_query_report(error, options));
        }
        let compiled = match compile_semantic(semantic, context) {
            Ok(compiled) => compiled,
            Err(error) => return Ok(self.rejected_read_only_query_report(error, options)),
        };
        let (task_id, outcome) = self.task_manager.submit_with_context_and_limits(
            Arc::new(compiled.program),
            authority,
            runtime_context,
            options.task_limits(),
        )?;
        Ok(self.read_only_query_report(Some(task_id), outcome, options))
    }

    fn rejected_read_only_query_report(
        &self,
        error: CompileError,
        options: ReadOnlySourceQueryOptions,
    ) -> ReadOnlySourceQueryReport {
        let diagnostic = render_source_task_error(
            &SourceTaskError::Compile(error),
            &self.identity_names(),
            &self.relation_names(),
            None,
            DiagnosticRenderOptions::default(),
        );
        let (rendered, rendered_truncated) =
            truncate_rendered_text(diagnostic.clone(), options.max_output_chars);
        ReadOnlySourceQueryReport {
            task_id: None,
            status: ReadOnlySourceQueryStatus::Rejected,
            value: None,
            error: None,
            diagnostics: vec![diagnostic],
            rendered,
            rendered_truncated,
        }
    }

    fn read_only_query_report(
        &self,
        task_id: Option<TaskId>,
        outcome: TaskOutcome,
        options: ReadOnlySourceQueryOptions,
    ) -> ReadOnlySourceQueryReport {
        read_only_query_report(
            task_id,
            outcome,
            options,
            &self.identity_names(),
            &self.relation_names(),
        )
    }

    fn identity_names(&self) -> BTreeMap<Identity, String> {
        let snapshot = self.task_manager.kernel().snapshot();
        snapshot
            .scan_relation(named_identity_relation(), &[None, None])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tuple| {
                let [name, identity] = tuple.values() else {
                    return None;
                };
                let name = name.as_symbol()?.name()?.to_owned();
                let identity = identity.as_identity()?;
                Some((identity, name))
            })
            .chain(
                snapshot
                    .rules()
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| (rule.id(), format!("rule{}", index + 1))),
            )
            .collect()
    }

    fn relation_names(&self) -> BTreeMap<Identity, String> {
        let snapshot = self.task_manager.kernel().snapshot();
        snapshot
            .relation_metadata()
            .filter_map(|metadata| Some((metadata.id(), metadata.name().name()?.to_owned())))
            .collect()
    }

    pub fn define_named_identity(&self, name: &str, identity: Identity) -> Result<(), String> {
        let symbol = Symbol::intern(name);
        let mut tx = self.task_manager.kernel().begin();
        tx.assert(
            named_identity_relation(),
            Tuple::from([Value::symbol(symbol), Value::identity(identity)]),
        )
        .map_err(|e| format!("assert failed: {e:?}"))?;
        tx.commit().map_err(|e| format!("commit failed: {e:?}"))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceChunk {
    text: String,
    start: usize,
}

fn source_chunks(source: &str) -> Vec<String> {
    source_chunks_with_offsets(source)
        .into_iter()
        .map(|chunk| chunk.text)
        .collect()
}

fn source_chunks_with_offsets(source: &str) -> Vec<SourceChunk> {
    let mut chunks = Vec::new();
    let mut buffer = String::new();
    let mut buffer_start = 0;
    let mut line_start = 0;

    for segment in source.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() && buffer.trim().is_empty() {
            line_start += segment.len();
            continue;
        }
        if buffer.is_empty() {
            buffer_start = line_start;
        }
        buffer.push_str(line);
        buffer.push('\n');
        if parse(&buffer).errors.is_empty() && source_has_items(&buffer) {
            chunks.push(SourceChunk {
                text: std::mem::take(&mut buffer),
                start: buffer_start,
            });
        }
        line_start += segment.len();
    }

    if !buffer.trim().is_empty() && source_has_items(&buffer) {
        chunks.push(SourceChunk {
            text: buffer,
            start: buffer_start,
        });
    }
    chunks
}

fn shift_source_task_error(error: SourceTaskError, offset: usize) -> SourceTaskError {
    match error {
        SourceTaskError::Compile(error) => {
            SourceTaskError::Compile(shift_compile_error(error, offset))
        }
        SourceTaskError::TaskManager(error) => SourceTaskError::TaskManager(error),
    }
}

fn return_compile_errors(errors: Vec<CompileError>) -> Result<(), SourceTaskError> {
    match errors.len() {
        0 => Ok(()),
        1 => Err(SourceTaskError::Compile(
            errors.into_iter().next().expect("one error exists"),
        )),
        _ => Err(SourceTaskError::Compile(CompileError::Diagnostics {
            errors,
        })),
    }
}

fn ensure_filein_replacement_completed(reports: &[RunReport]) -> Result<(), SourceTaskError> {
    for report in reports {
        match &report.outcome {
            TaskOutcome::Complete { .. } => {}
            TaskOutcome::Aborted { error, .. } => {
                return Err(
                    TaskManagerError::Task(TaskError::Runtime(RuntimeError::Raised(error.clone())))
                        .into(),
                );
            }
            TaskOutcome::Suspended { .. } => {
                return Err(TaskManagerError::Task(TaskError::Runtime(
                    RuntimeError::InvalidBuiltinCall {
                        name: Symbol::intern("filein"),
                        message: "unit replacement cannot suspend".to_owned(),
                    },
                ))
                .into());
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GrantKind {
    Actor,
    Role,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum GrantOp {
    Read,
    Write,
    Invoke,
    Effect,
}

impl GrantOp {
    fn from_section(text: &str) -> Option<Self> {
        match text {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "invoke" => Some(Self::Invoke),
            "effect" => Some(Self::Effect),
            _ => None,
        }
    }

    fn relation_name(self, kind: GrantKind) -> &'static str {
        match (kind, self) {
            (GrantKind::Actor, Self::Read) => "CanRead",
            (GrantKind::Actor, Self::Write) => "CanWrite",
            (GrantKind::Actor, Self::Invoke) => "CanInvoke",
            (GrantKind::Actor, Self::Effect) => "CanEffect",
            (GrantKind::Role, Self::Read) => "RoleCanRead",
            (GrantKind::Role, Self::Write) => "RoleCanWrite",
            (GrantKind::Role, Self::Invoke) => "RoleCanInvoke",
            (GrantKind::Role, Self::Effect) => "RoleCanEffect",
        }
    }

    fn source_name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Invoke => "invoke",
            Self::Effect => "effect",
        }
    }
}

struct GrantHeader<'a> {
    kind: GrantKind,
    subject: &'a str,
}

fn expand_filein_grant_blocks(source: &str) -> Result<String, SourceTaskError> {
    let mut output = String::new();
    let mut lines = source.lines();

    while let Some(line) = lines.next() {
        let Some(header) = parse_grant_header(line) else {
            output.push_str(line);
            output.push('\n');
            continue;
        };

        let mut current_op = None;
        let mut closed = false;
        for body_line in lines.by_ref() {
            let trimmed = body_line.trim();
            if trimmed == "end" {
                closed = true;
                break;
            }
            if trimmed.is_empty() || trimmed.starts_with("//") {
                output.push_str(body_line);
                output.push('\n');
                continue;
            }

            if let Some((op, rest)) = parse_grant_op_line(trimmed) {
                current_op = Some(op);
                emit_grant_assertions(&mut output, header.kind, header.subject, op, rest)?;
                continue;
            }

            if let Some(op) = current_op {
                emit_grant_assertions(&mut output, header.kind, header.subject, op, trimmed)?;
                continue;
            }

            return Err(grant_parse_error(format!(
                "expected read:, write:, invoke:, effect, or end in grant block, got {trimmed:?}"
            )));
        }

        if !closed {
            return Err(grant_parse_error("unterminated grant block"));
        }
    }

    Ok(output)
}

fn parse_grant_header(line: &str) -> Option<GrantHeader<'_>> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix("grant ")?;
    let (kind, subject) = if let Some(subject) = rest.strip_prefix("role ") {
        (GrantKind::Role, subject.trim())
    } else {
        (GrantKind::Actor, rest.trim())
    };
    (!subject.is_empty()).then_some(GrantHeader { kind, subject })
}

fn parse_grant_op_line(line: &str) -> Option<(GrantOp, &str)> {
    for name in ["read", "write", "invoke", "effect"] {
        let op = GrantOp::from_section(name).expect("static grant op is valid");
        if line == name {
            return Some((op, ""));
        }
        if let Some(rest) = line.strip_prefix(&format!("{name}:")) {
            return Some((op, rest.trim()));
        }
        if let Some(rest) = line.strip_prefix(name)
            && rest
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_whitespace())
        {
            return Some((op, rest.trim()));
        }
    }
    None
}

fn emit_grant_assertions(
    output: &mut String,
    kind: GrantKind,
    subject: &str,
    op: GrantOp,
    rest: &str,
) -> Result<(), SourceTaskError> {
    let relation = op.relation_name(kind);
    if op == GrantOp::Effect {
        if !rest.trim().is_empty() {
            return Err(grant_parse_error("effect grants do not take targets"));
        }
        output.push_str(&format!("assert {relation}({subject})\n"));
        return Ok(());
    }

    let targets = parse_grant_targets(rest)?;
    for target in targets {
        output.push_str(&format!("assert {relation}({subject}, {target})\n"));
    }
    Ok(())
}

fn parse_grant_targets(rest: &str) -> Result<Vec<&str>, SourceTaskError> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    let targets = rest
        .split(',')
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Err(grant_parse_error(
            "grant operation requires at least one target",
        ));
    }
    Ok(targets)
}

fn grant_parse_error(message: impl Into<String>) -> SourceTaskError {
    unsupported_runner_error(NodeId(0), None, message.into())
}

fn shift_compile_error(error: CompileError, offset: usize) -> CompileError {
    match error {
        CompileError::Diagnostics { errors } => CompileError::Diagnostics {
            errors: errors
                .into_iter()
                .map(|error| shift_compile_error(error, offset))
                .collect(),
        },
        CompileError::ParseErrors { errors } => CompileError::ParseErrors {
            errors: errors
                .into_iter()
                .map(|mut error| {
                    error.span = shift_span(error.span, offset);
                    error
                })
                .collect(),
        },
        CompileError::SemanticDiagnostic { mut diagnostic } => {
            diagnostic.span = shift_span(diagnostic.span, offset);
            CompileError::SemanticDiagnostic { diagnostic }
        }
        CompileError::Unsupported {
            node,
            span,
            message,
        } => CompileError::Unsupported {
            node,
            span: span.map(|span| shift_span(span, offset)),
            message,
        },
        CompileError::UnknownRelation { node, span, name } => CompileError::UnknownRelation {
            node,
            span: span.map(|span| shift_span(span, offset)),
            name,
        },
        CompileError::UnknownIdentity { node, span, name } => CompileError::UnknownIdentity {
            node,
            span: span.map(|span| shift_span(span, offset)),
            name,
        },
        CompileError::UnknownValue { node, span, name } => CompileError::UnknownValue {
            node,
            span: span.map(|span| shift_span(span, offset)),
            name,
        },
        CompileError::InvalidLiteral {
            node,
            span,
            message,
        } => CompileError::InvalidLiteral {
            node,
            span: span.map(|span| shift_span(span, offset)),
            message,
        },
        CompileError::UnboundLocal {
            node,
            span,
            binding,
        } => CompileError::UnboundLocal {
            node,
            span: span.map(|span| shift_span(span, offset)),
            binding,
        },
        CompileError::ValueKindMismatch {
            node,
            span,
            subject,
            expected,
            inferred,
        } => CompileError::ValueKindMismatch {
            node,
            span: span.map(|span| shift_span(span, offset)),
            subject,
            expected,
            inferred,
        },
        CompileError::StaticTypeMismatch {
            node,
            span,
            subject,
            expected,
            inferred,
        } => CompileError::StaticTypeMismatch {
            node,
            span: span.map(|span| shift_span(span, offset)),
            subject,
            expected,
            inferred,
        },
        CompileError::FunctionResultKindMismatch {
            node,
            span,
            function,
            expected,
            inferred,
        } => CompileError::FunctionResultKindMismatch {
            node,
            span: span.map(|span| shift_span(span, offset)),
            function,
            expected,
            inferred,
        },
        CompileError::VerbResultKindMismatch {
            node,
            span,
            selector,
            expected,
            inferred,
        } => CompileError::VerbResultKindMismatch {
            node,
            span: span.map(|span| shift_span(span, offset)),
            selector,
            expected,
            inferred,
        },
        CompileError::ParameterKindMismatch {
            node,
            span,
            parameter,
            expected,
            inferred,
        } => CompileError::ParameterKindMismatch {
            node,
            span: span.map(|span| shift_span(span, offset)),
            parameter,
            expected,
            inferred,
        },
        CompileError::ParameterDefaultKindMismatch {
            node,
            span,
            parameter,
            expected,
            inferred,
        } => CompileError::ParameterDefaultKindMismatch {
            node,
            span: span.map(|span| shift_span(span, offset)),
            parameter,
            expected,
            inferred,
        },
        CompileError::MissingOptionalParameterDefault {
            node,
            span,
            parameter,
        } => CompileError::MissingOptionalParameterDefault {
            node,
            span: span.map(|span| shift_span(span, offset)),
            parameter,
        },
        CompileError::InvalidRestParameterKind {
            node,
            span,
            parameter,
            declared,
        } => CompileError::InvalidRestParameterKind {
            node,
            span: span.map(|span| shift_span(span, offset)),
            parameter,
            declared,
        },
        CompileError::Runtime(error) => CompileError::Runtime(error),
        CompileError::Kernel(error) => CompileError::Kernel(error),
    }
}

fn shift_span(span: Span, offset: usize) -> Span {
    span.start + offset..span.end + offset
}

fn expand_filein_text_includes(
    source: &str,
    load_include: &mut impl FnMut(&str) -> Result<String, String>,
) -> Result<String, SourceTaskError> {
    let mut output = String::new();
    let mut index = 0;
    let bytes = source.as_bytes();
    while index < bytes.len() {
        if bytes[index] == b'"' {
            let end = skip_string_literal(source, index)?;
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            let end = source[index..]
                .find('\n')
                .map(|offset| index + offset + 1)
                .unwrap_or(source.len());
            output.push_str(&source[index..end]);
            index = end;
            continue;
        }
        if include_text_starts_at(source, index) {
            let (end, path) = parse_include_text_call(source, index)?;
            let text = load_include(&path).map_err(|error| {
                unsupported_runner_error(
                    NodeId(0),
                    None,
                    format!("failed to include text file {path:?}: {error}"),
                )
            })?;
            output.push_str(&format_filein_string_literal(&text));
            index = end;
            continue;
        }
        let ch = source[index..].chars().next().unwrap();
        output.push(ch);
        index += ch.len_utf8();
    }
    Ok(output)
}

fn include_text_starts_at(source: &str, index: usize) -> bool {
    const NAME: &str = "include_text";
    source[index..].starts_with(NAME)
        && (index == 0 || !is_identifier_byte(source.as_bytes()[index - 1]))
        && source
            .as_bytes()
            .get(index + NAME.len())
            .is_some_and(|byte| !is_identifier_byte(*byte))
}

fn parse_include_text_call(source: &str, start: usize) -> Result<(usize, String), SourceTaskError> {
    const NAME: &str = "include_text";
    let mut index = start + NAME.len();
    index = skip_ascii_space(source, index);
    if source.as_bytes().get(index) != Some(&b'(') {
        return Err(include_text_parse_error());
    }
    index += 1;
    index = skip_ascii_space(source, index);
    if source.as_bytes().get(index) != Some(&b'"') {
        return Err(include_text_parse_error());
    }
    let (next, path) = parse_filein_string_literal(source, index)?;
    index = skip_ascii_space(source, next);
    if source.as_bytes().get(index) != Some(&b')') {
        return Err(include_text_parse_error());
    }
    Ok((index + 1, path))
}

fn include_text_parse_error() -> SourceTaskError {
    unsupported_runner_error(NodeId(0), None, "expected include_text(\"relative/path\")")
}

fn skip_ascii_space(source: &str, mut index: usize) -> usize {
    while source
        .as_bytes()
        .get(index)
        .is_some_and(u8::is_ascii_whitespace)
    {
        index += 1;
    }
    index
}

fn skip_string_literal(source: &str, start: usize) -> Result<usize, SourceTaskError> {
    parse_filein_string_literal(source, start).map(|(end, _)| end)
}

fn parse_filein_string_literal(
    source: &str,
    start: usize,
) -> Result<(usize, String), SourceTaskError> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    let mut value = String::new();
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok((index + 1, value)),
            b'\\' => {
                index += 1;
                let Some(escaped) = bytes.get(index).copied() else {
                    return Err(unterminated_string_error());
                };
                let ch = match escaped {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    other => other as char,
                };
                value.push(ch);
                index += 1;
            }
            _ => {
                let ch = source[index..].chars().next().unwrap();
                value.push(ch);
                index += ch.len_utf8();
            }
        }
    }
    Err(unterminated_string_error())
}

fn unterminated_string_error() -> SourceTaskError {
    unsupported_runner_error(
        NodeId(0),
        None,
        "unterminated string literal in filein source",
    )
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn format_filein_string_literal(text: &str) -> String {
    format!("{text:?}")
}

fn source_has_items(source: &str) -> bool {
    !parse_semantic(source).hir.items.is_empty()
}

fn root_source_needs_install_chunking(source: &str) -> bool {
    let semantic = parse_semantic(source);
    if !semantic.parse_errors.is_empty() || !semantic.diagnostics.is_empty() {
        return false;
    }

    let has_method = semantic
        .hir
        .items
        .iter()
        .any(|item| matches!(item, HirItem::Method { .. }));
    let has_rule = semantic
        .hir
        .items
        .iter()
        .any(|item| matches!(item, HirItem::RelationRule { .. }));
    let has_executable = semantic
        .hir
        .items
        .iter()
        .any(|item| !matches!(item, HirItem::Method { .. } | HirItem::RelationRule { .. }));

    (has_method || has_rule) && (has_executable || (has_method && has_rule))
}

fn compile_context_from_catalog(
    kernel: &RelationKernel,
    builtins: &BuiltinRegistry,
) -> CompileContext {
    let snapshot = kernel.snapshot();
    let mut context = CompileContext::new().with_method_relations(method_relations());
    for (name, result) in builtins.result_kinds() {
        if let Some(name) = name.name() {
            context.define_runtime_function(name, result);
        }
    }
    for metadata in snapshot.relation_metadata() {
        context.define_relation_metadata(metadata.clone());
    }
    for tuple in snapshot
        .scan(named_identity_relation(), &[None, None])
        .unwrap_or_default()
    {
        let [name, identity] = tuple.values() else {
            continue;
        };
        let (Some(name), Some(identity)) = (
            name.as_symbol().and_then(Symbol::name),
            identity.as_identity(),
        ) else {
            continue;
        };
        context.define_identity(name, identity);
    }
    for (index, rule) in snapshot.rules().iter().enumerate() {
        context.define_identity(format!("rule{}", index + 1), rule.id());
    }
    context
}

fn apply_host_request_functions(
    context: &mut CompileContext,
    functions: &[(String, HostRequestFunction)],
) {
    for (name, function) in functions {
        context.define_host_request_function(name.clone(), function.clone());
    }
}

fn default_host_request_functions(
    embedding_provider: EmbeddingProviderKind,
) -> Vec<(String, HostRequestFunction)> {
    let mut functions = embedding::host_request_functions(embedding_provider);
    functions.extend(openai::host_request_functions());
    functions.push((
        "mica_query".to_owned(),
        HostRequestFunction {
            service: Symbol::intern("mica_query"),
            payload_fields: vec![Symbol::intern("query"), Symbol::intern("options")],
            timeout: Some(Value::int(5).expect("static timeout should fit in mica int")),
        },
    ));
    functions
}

fn predeclare_source_names_in_kernel(
    kernel: &RelationKernel,
    semantic: &mica_compiler::SemanticProgram,
) -> Result<(), SourceTaskError> {
    let mut declarations = SourceDeclarations::default();
    for item in &semantic.hir.items {
        let HirItem::Expr { expr, .. } = item else {
            continue;
        };
        collect_declaration_expr(expr, &mut declarations);
    }
    if declarations.identities.is_empty() && declarations.relations.is_empty() {
        return Ok(());
    }

    let mut tx = kernel.begin();
    let mut next_identity_id = next_generated_identity_id(kernel);
    for identity_name in declarations.identities {
        ensure_runtime_named_identity(&mut tx, &identity_name, &mut next_identity_id)?;
    }
    tx.commit().map_err(CompileError::from)?;

    for declaration in declarations.relations {
        ensure_declared_relation(kernel, declaration)?;
    }
    Ok(())
}

fn collect_source_declarations(source: &str) -> Result<SourceDeclarations, SourceTaskError> {
    let source = expand_filein_grant_blocks(source)?;
    let parsed = parse(&source);
    if !parsed.errors.is_empty() {
        return Err(CompileError::ParseErrors {
            errors: parsed.errors,
        }
        .into());
    }
    let mut declarations = SourceDeclarations::default();
    for chunk in source_chunks(&source) {
        let semantic = parse_semantic(&chunk);
        if !semantic.parse_errors.is_empty() {
            return Err(CompileError::ParseErrors {
                errors: semantic.parse_errors.clone(),
            }
            .into());
        }
        if let Some(diagnostic) = semantic.diagnostics.first() {
            return Err(CompileError::SemanticDiagnostic {
                diagnostic: diagnostic.clone(),
            }
            .into());
        }
        for item in semantic.hir.items {
            let HirItem::Expr { expr, .. } = item else {
                continue;
            };
            collect_declaration_expr(&expr, &mut declarations);
        }
    }
    Ok(declarations)
}

fn collect_declaration_expr(expr: &HirExpr, declarations: &mut SourceDeclarations) {
    let HirExpr::Call { callee, args, .. } = expr else {
        return;
    };
    let HirExpr::ExternalRef { name, .. } = callee.as_ref() else {
        return;
    };
    match name.as_str() {
        "make_identity" => {
            let [arg] = args.as_slice() else {
                return;
            };
            if let HirExpr::Symbol { name, .. } = &arg.value {
                declarations.identities.insert(name.clone());
            }
        }
        "make_relation" => {
            if !matches!(args.len(), 2 | 3) {
                return;
            }
            let Some((name, arity)) = hir_relation_name_and_arity(args) else {
                return;
            };
            let Some(durability) = hir_relation_durability(args.get(2).map(|arg| &arg.value))
            else {
                return;
            };
            declarations.relations.push(SourceRelationDeclaration {
                name,
                arity,
                conflict_policy: ConflictPolicy::Set,
                durability,
            });
        }
        "make_functional_relation" => {
            if !matches!(args.len(), 3 | 4) {
                return;
            }
            let Some((name, arity)) = hir_relation_name_and_arity(args) else {
                return;
            };
            let Some(key_positions) = hir_key_positions(&args[2].value, arity) else {
                return;
            };
            let Some(durability) = hir_relation_durability(args.get(3).map(|arg| &arg.value))
            else {
                return;
            };
            declarations.relations.push(SourceRelationDeclaration {
                name,
                arity,
                conflict_policy: ConflictPolicy::Functional { key_positions },
                durability,
            });
        }
        _ => {}
    }
}

fn hir_relation_name_and_arity(args: &[HirArg]) -> Option<(String, u16)> {
    let (HirExpr::Symbol { name, .. }, HirExpr::Literal { value, .. }) =
        (&args.first()?.value, &args.get(1)?.value)
    else {
        return None;
    };
    let Literal::Int(arity) = value else {
        return None;
    };
    Some((name.clone(), arity.parse().ok()?))
}

fn hir_relation_durability(expr: Option<&HirExpr>) -> Option<RelationDurability> {
    let Some(expr) = expr else {
        return Some(RelationDurability::Durable);
    };
    let HirExpr::Symbol { name, .. } = expr else {
        return None;
    };
    match name.as_str() {
        "durable" => Some(RelationDurability::Durable),
        "volatile" => Some(RelationDurability::Volatile),
        _ => None,
    }
}

fn hir_key_positions(expr: &HirExpr, arity: u16) -> Option<Vec<u16>> {
    let HirExpr::List { items, .. } = expr else {
        return None;
    };
    items
        .iter()
        .map(|item| {
            let HirCollectionItem::Expr(HirExpr::Literal {
                value: Literal::Int(value),
                ..
            }) = item
            else {
                return None;
            };
            let position = value.parse::<u16>().ok()?;
            (position < arity).then_some(position)
        })
        .collect()
}

fn is_exported_fact_relation(relation: Identity) -> bool {
    !matches!(
        relation.raw(),
        NAMED_IDENTITY_RELATION_ID
            | METHOD_SELECTOR_RELATION_ID
            | PARAM_RELATION_ID
            | METHOD_PROGRAM_RELATION_ID
            | PROGRAM_BYTES_RELATION_ID
            | METHOD_SOURCE_RELATION_ID
            | SOURCE_OWNS_FACT_RELATION_ID
            | SOURCE_OWNS_RULE_RELATION_ID
            | SOURCE_OWNS_RELATION_RELATION_ID
    )
}

fn is_ownable_fact_relation(relation: Identity) -> bool {
    !matches!(
        relation.raw(),
        SOURCE_OWNS_FACT_RELATION_ID
            | SOURCE_OWNS_RULE_RELATION_ID
            | SOURCE_OWNS_RELATION_RELATION_ID
    )
}

fn is_read_only_system_relation(relation: Identity) -> bool {
    matches!(
        relation.raw(),
        RELATION_RELATION_ID
            | RELATION_NAME_RELATION_ID
            | ARITY_RELATION_ID
            | RULE_RELATION_ID
            | RULE_HEAD_RELATION_ID
            | RULE_SOURCE_RELATION_ID
            | ACTIVE_RULE_RELATION_ID
            | ARGUMENT_NAME_RELATION_ID
            | CONFLICT_POLICY_RELATION_ID
            | FUNCTIONAL_KEY_RELATION_ID
            | INDEX_RELATION_ID
            | INDEX_POSITION_RELATION_ID
            | INDEX_STORAGE_KIND_RELATION_ID
            | SUBJECT_FACT_RELATION_ID
            | MENTIONED_FACT_RELATION_ID
            | EXTENSIONAL_MENTIONED_FACT_RELATION_ID
            | RELATION_DURABILITY_RELATION_ID
    )
}

fn ownership_fact_tuple(unit: Symbol, relation: Identity, tuple: Tuple) -> Tuple {
    Tuple::from([
        Value::symbol(unit),
        Value::identity(relation),
        Value::list(tuple.values().to_vec()),
    ])
}

fn owned_fact_tuple(ownership: &Tuple) -> Option<(Identity, Tuple)> {
    let [_, relation, values] = ownership.values() else {
        return None;
    };
    let relation = relation.as_identity()?;
    let tuple = values.with_list(|values| Tuple::new(values.iter().cloned()))?;
    Some((relation, tuple))
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GrantBlockKey {
    kind: GrantKind,
    subject: String,
}

type GrantBlocks = BTreeMap<GrantBlockKey, BTreeMap<GrantOp, BTreeSet<String>>>;

fn grant_fact_source(
    relation_name: &str,
    tuple: &Tuple,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> Option<(GrantBlockKey, GrantOp, Option<String>)> {
    let (kind, op) = match relation_name {
        "CanRead" => (GrantKind::Actor, GrantOp::Read),
        "CanWrite" => (GrantKind::Actor, GrantOp::Write),
        "CanInvoke" => (GrantKind::Actor, GrantOp::Invoke),
        "CanEffect" => (GrantKind::Actor, GrantOp::Effect),
        "RoleCanRead" => (GrantKind::Role, GrantOp::Read),
        "RoleCanWrite" => (GrantKind::Role, GrantOp::Write),
        "RoleCanInvoke" => (GrantKind::Role, GrantOp::Invoke),
        "RoleCanEffect" => (GrantKind::Role, GrantOp::Effect),
        _ => return None,
    };
    let values = tuple.values();
    let subject = values.first()?;
    let key = GrantBlockKey {
        kind,
        subject: source_literal(subject, identity_names, relation_names),
    };
    if op == GrantOp::Effect {
        return (values.len() == 1).then_some((key, op, None));
    }
    let target = values.get(1)?;
    (values.len() == 2).then_some((
        key,
        op,
        Some(source_literal(target, identity_names, relation_names)),
    ))
}

fn render_grant_blocks(grants: GrantBlocks) -> String {
    grants
        .into_iter()
        .map(|(key, ops)| {
            let mut out = match key.kind {
                GrantKind::Actor => format!("grant {}", key.subject),
                GrantKind::Role => format!("grant role {}", key.subject),
            };
            out.push('\n');
            for op in [
                GrantOp::Read,
                GrantOp::Write,
                GrantOp::Invoke,
                GrantOp::Effect,
            ] {
                let Some(targets) = ops.get(&op) else {
                    continue;
                };
                if op == GrantOp::Effect {
                    out.push_str("  effect\n");
                    continue;
                }
                out.push_str("  ");
                out.push_str(op.source_name());
                out.push_str(":\n");
                for target in targets {
                    out.push_str("    ");
                    out.push_str(target);
                    out.push('\n');
                }
            }
            out.push_str("end");
            out
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn fileout_unit_source(kernel: &RelationKernel, unit: Symbol) -> Result<String, KernelError> {
    let snapshot = kernel.snapshot();
    let identity_names = identity_name_map(snapshot.as_ref())?;
    let relation_names = relation_name_map(&snapshot);
    let mut relation_declarations = BTreeSet::new();
    let mut identity_declarations = BTreeSet::new();
    let mut facts = BTreeSet::new();
    let mut grants: GrantBlocks = BTreeMap::new();
    let mut rule_sources = BTreeSet::new();
    let mut method_sources = BTreeSet::new();

    for row in snapshot.scan(
        source_owns_relation_relation(),
        &[Some(Value::symbol(unit)), None],
    )? {
        let Some(relation) = row.values().get(1).and_then(Value::as_identity) else {
            continue;
        };
        if let Some(metadata) = snapshot
            .relation_metadata()
            .find(|metadata| metadata.id() == relation)
            && let Some(declaration) = relation_declaration_source(metadata)
        {
            relation_declarations.insert(declaration);
        }
    }

    for row in snapshot.scan(
        source_owns_fact_relation(),
        &[Some(Value::symbol(unit)), None, None],
    )? {
        let Some((relation, tuple)) = owned_fact_tuple(&row) else {
            continue;
        };
        if relation == named_identity_relation() {
            if let [name, _identity] = tuple.values()
                && let Some(symbol) = name.as_symbol()
                && let Some(name) = symbol.name()
            {
                identity_declarations.insert(name.to_owned());
            }
            continue;
        }
        if relation == method_source_relation() {
            if let Some(source) = tuple
                .values()
                .get(1)
                .and_then(|value| value.with_str(str::to_owned))
            {
                method_sources.insert(source.trim().to_owned());
            }
            continue;
        }
        if is_exported_fact_relation(relation) {
            if let Some(relation_name) = relation_names.get(&relation)
                && let Some((key, op, target)) =
                    grant_fact_source(relation_name, &tuple, &identity_names, &relation_names)
            {
                let targets = grants.entry(key).or_default().entry(op).or_default();
                if let Some(target) = target {
                    targets.insert(target);
                }
                continue;
            }
            facts.insert((relation, tuple));
        }
    }

    for row in snapshot.scan(
        source_owns_rule_relation(),
        &[Some(Value::symbol(unit)), None],
    )? {
        let Some(rule_id) = row.values().get(1).and_then(Value::as_identity) else {
            continue;
        };
        if let Some(rule) = snapshot
            .rules()
            .iter()
            .find(|rule| rule.id() == rule_id && rule.active())
        {
            rule_sources.insert(rule.source().trim().to_owned());
        }
    }

    let mut sections = Vec::new();
    if !identity_declarations.is_empty() {
        sections.push(
            identity_declarations
                .into_iter()
                .map(|name| format!("make_identity(:{name})"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if !relation_declarations.is_empty() {
        sections.push(
            relation_declarations
                .into_iter()
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if !facts.is_empty() {
        sections.push(
            facts
                .into_iter()
                .filter_map(|(relation, tuple)| {
                    let relation_name = relation_names.get(&relation)?;
                    Some(format!(
                        "assert {relation_name}({})",
                        tuple
                            .values()
                            .iter()
                            .map(|value| source_literal(value, &identity_names, &relation_names))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if !grants.is_empty() {
        sections.push(render_grant_blocks(grants));
    }
    if !rule_sources.is_empty() {
        sections.push(rule_sources.into_iter().collect::<Vec<_>>().join("\n\n"));
    }
    if !method_sources.is_empty() {
        sections.push(method_sources.into_iter().collect::<Vec<_>>().join("\n\n"));
    }

    Ok(sections.join("\n\n"))
}

fn relation_declaration_source(metadata: &RelationMetadata) -> Option<String> {
    let name = metadata.name().name()?;
    let durability = match metadata.durability() {
        RelationDurability::Durable => "",
        RelationDurability::Volatile => ", :volatile",
    };
    Some(match metadata.conflict_policy() {
        ConflictPolicy::Set => format!("make_relation(:{name}, {}{durability})", metadata.arity()),
        ConflictPolicy::Functional { key_positions } => format!(
            "make_functional_relation(:{name}, {}, [{}]{durability})",
            metadata.arity(),
            key_positions
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ConflictPolicy::EventAppend => {
            format!("make_relation(:{name}, {}{durability})", metadata.arity())
        }
    })
}

fn identity_name_map(
    reader: &impl RelationRead,
) -> Result<BTreeMap<Identity, String>, KernelError> {
    Ok(reader
        .scan_relation(named_identity_relation(), &[None, None])?
        .into_iter()
        .filter_map(|tuple| {
            let [name, identity] = tuple.values() else {
                return None;
            };
            Some((
                identity.as_identity()?,
                name.as_symbol()?.name()?.to_owned(),
            ))
        })
        .collect())
}

fn relation_name_map(snapshot: &mica_relation_kernel::Snapshot) -> BTreeMap<Identity, String> {
    snapshot
        .relation_metadata()
        .filter_map(|metadata| Some((metadata.id(), metadata.name().name()?.to_owned())))
        .collect()
}

fn source_literal(
    value: &Value,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    if value.is_empty_relation() {
        return "nothing".to_owned();
    }
    match value.kind() {
        ValueKind::Bool => value.as_bool().unwrap().to_string(),
        ValueKind::Int => value.as_int().unwrap().to_string(),
        ValueKind::Float => float_to_literal(value.as_float().unwrap()),
        ValueKind::Identity => {
            let identity = value.as_identity().unwrap();
            match identity_names.get(&identity) {
                Some(name) => format!("#{name}"),
                None => match relation_names.get(&identity) {
                    Some(name) => format!(":{name}"),
                    None => format!("#{}", identity.raw()),
                },
            }
        }
        ValueKind::Symbol => render_symbol(value.as_symbol().unwrap(), ":"),
        ValueKind::ErrorCode => render_symbol(value.as_error_code().unwrap(), ""),
        ValueKind::String => value.with_str(|value| format!("{value:?}")).unwrap(),
        ValueKind::Bytes => bytes_literal(value),
        ValueKind::List => value
            .with_list(|values| {
                render_sequence(
                    "[",
                    "]",
                    values
                        .iter()
                        .map(|value| source_literal(value, identity_names, relation_names)),
                )
            })
            .unwrap(),
        ValueKind::Map => value
            .with_map(|entries| {
                render_sequence(
                    "{",
                    "}",
                    entries.iter().map(|(key, value)| {
                        format!(
                            "{} -> {}",
                            source_literal(key, identity_names, relation_names),
                            source_literal(value, identity_names, relation_names)
                        )
                    }),
                )
            })
            .unwrap(),
        ValueKind::Range => value
            .with_range(|start, end| match end {
                Some(end) => format!(
                    "{}..{}",
                    source_literal(start, identity_names, relation_names),
                    source_literal(end, identity_names, relation_names)
                ),
                None => format!(
                    "{}.._",
                    source_literal(start, identity_names, relation_names)
                ),
            })
            .unwrap(),
        ValueKind::Error => value
            .with_error(|error| {
                let mut out = format!("error({}", render_symbol(error.code(), ""));
                if let Some(message) = error.message() {
                    out.push_str(", ");
                    out.push_str(&format!("{message:?}"));
                }
                if let Some(payload) = error.value() {
                    if error.message().is_none() {
                        out.push_str(", nothing");
                    }
                    out.push_str(", ");
                    out.push_str(&source_literal(payload, identity_names, relation_names));
                }
                out.push(')');
                out
            })
            .unwrap(),
        ValueKind::Capability => "<cap>".to_owned(),
        ValueKind::Function => "<function>".to_owned(),
        ValueKind::Relation => value
            .with_relation(|relation| {
                let heading = render_sequence(
                    "[",
                    "]",
                    relation
                        .heading()
                        .iter()
                        .map(|column| render_symbol(*column, ":")),
                );
                let rows = render_sequence(
                    "{",
                    "}",
                    relation.rows().iter().map(|row| {
                        render_sequence(
                            "[",
                            "]",
                            row.values()
                                .iter()
                                .map(|cell| source_literal(cell, identity_names, relation_names)),
                        )
                    }),
                );
                format!("{heading} {rows}")
            })
            .unwrap(),
        ValueKind::Frob => value
            .with_frob(|delegate, payload| {
                format!(
                    "{}<{}>",
                    source_literal(&Value::identity(delegate), identity_names, relation_names),
                    source_literal(payload, identity_names, relation_names)
                )
            })
            .unwrap(),
    }
}

fn read_only_query_report(
    task_id: Option<TaskId>,
    outcome: TaskOutcome,
    options: ReadOnlySourceQueryOptions,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> ReadOnlySourceQueryReport {
    let (status, value, error, rendered_value) = match outcome {
        TaskOutcome::Complete { value, .. } => {
            let rendered = source_literal(&value, identity_names, relation_names);
            (
                ReadOnlySourceQueryStatus::Complete,
                Some(value),
                None,
                rendered,
            )
        }
        TaskOutcome::Aborted { error, .. } => {
            let rendered = source_literal(&error, identity_names, relation_names);
            (
                ReadOnlySourceQueryStatus::Aborted,
                None,
                Some(error),
                rendered,
            )
        }
        TaskOutcome::Suspended { kind, .. } => (
            ReadOnlySourceQueryStatus::Suspended,
            None,
            None,
            format!("query suspended: {kind:?}"),
        ),
    };
    let (rendered, rendered_truncated) =
        truncate_rendered_text(rendered_value, options.max_output_chars);
    ReadOnlySourceQueryReport {
        task_id,
        status,
        value,
        error,
        diagnostics: Vec::new(),
        rendered,
        rendered_truncated,
    }
}

fn truncate_rendered_text(text: String, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (String::new(), !text.is_empty());
    }
    if text.chars().count() <= max_chars {
        return (text, false);
    }
    let mut rendered = text.chars().take(max_chars).collect::<String>();
    rendered.push_str("\n... truncated");
    (rendered, true)
}

fn installed_rule_value(rules: &[mica_relation_kernel::RuleDefinition]) -> Value {
    match rules {
        [rule] => Value::identity(rule.id()),
        _ => Value::list(
            rules
                .iter()
                .map(|rule| Value::identity(rule.id()))
                .collect::<Vec<_>>(),
        ),
    }
}

fn installed_method_value(installation: &MethodInstallation) -> Value {
    match installation.methods.as_slice() {
        [method] => method.method.clone(),
        methods => Value::list(
            methods
                .iter()
                .map(|method| method.method.clone())
                .collect::<Vec<_>>(),
        ),
    }
}

fn assign_generated_verb_identities(
    semantic: &mut mica_compiler::SemanticProgram,
    next_identity_id: u64,
) -> Result<(), CompileError> {
    let mut generated = 0;
    for item in &mut semantic.hir.items {
        let HirItem::Method {
            id,
            kind,
            identity,
            selector,
            ..
        } = item
        else {
            continue;
        };
        if !matches!(kind, MethodKind::Verb) || identity.is_some() {
            continue;
        }
        let selector = selector.as_ref().ok_or_else(|| CompileError::Unsupported {
            node: *id,
            span: None,
            message: "verb installation requires a selector name".to_owned(),
        })?;
        let ordinal = next_identity_id
            .checked_sub(GENERATED_METHOD_ID_START)
            .map(|offset| offset / 2 + 1 + generated)
            .unwrap_or(1 + generated);
        generated += 1;
        *identity = Some(format!("verb_{selector}_{ordinal}"));
    }
    Ok(())
}

fn ensure_named_identity(
    tx: &mut mica_relation_kernel::Transaction<'_>,
    name: &str,
    next_identity_id: &mut u64,
) -> Result<Identity, CompileError> {
    let symbol = Symbol::intern(name);
    let tuples = tx.scan(
        named_identity_relation(),
        &[Some(Value::symbol(symbol)), None],
    )?;
    if let Some(identity) = tuples
        .first()
        .and_then(|tuple| tuple.values().get(1))
        .and_then(Value::as_identity)
    {
        return Ok(identity);
    }

    let identity = loop {
        let Some(identity) = (*next_identity_id < GENERATED_IDENTITY_ID_START)
            .then(|| Identity::new(*next_identity_id))
            .flatten()
        else {
            return Err(CompileError::Unsupported {
                node: mica_compiler::NodeId(0),
                span: None,
                message: "generated method identity exhausted".to_owned(),
            });
        };
        *next_identity_id += 1;
        if tx
            .scan(
                named_identity_relation(),
                &[None, Some(Value::identity(identity))],
            )?
            .is_empty()
        {
            break identity;
        }
    };
    tx.assert(
        named_identity_relation(),
        Tuple::from([Value::symbol(symbol), Value::identity(identity)]),
    )?;
    Ok(identity)
}

fn ensure_runtime_named_identity(
    tx: &mut mica_relation_kernel::Transaction<'_>,
    name: &str,
    next_identity_id: &mut u64,
) -> Result<Identity, CompileError> {
    let symbol = Symbol::intern(name);
    if let Some(identity) = identity_named_in_tx(tx, symbol)? {
        return Ok(identity);
    }

    let identity = loop {
        let Some(identity) = (*next_identity_id < GENERATED_RELATION_ID_START)
            .then(|| Identity::new(*next_identity_id))
            .flatten()
        else {
            return Err(CompileError::Unsupported {
                node: mica_compiler::NodeId(0),
                span: None,
                message: "generated identity exhausted".to_owned(),
            });
        };
        *next_identity_id += 1;
        if tx
            .scan(
                named_identity_relation(),
                &[None, Some(Value::identity(identity))],
            )?
            .is_empty()
        {
            break identity;
        }
    };
    tx.assert(
        named_identity_relation(),
        Tuple::from([Value::symbol(symbol), Value::identity(identity)]),
    )?;
    Ok(identity)
}

fn ensure_declared_relation(
    kernel: &RelationKernel,
    declaration: SourceRelationDeclaration,
) -> Result<Identity, SourceTaskError> {
    let name = Symbol::intern(&declaration.name);
    if let Some(metadata) = relation_metadata_named(kernel, name) {
        if metadata.arity() == declaration.arity
            && metadata.conflict_policy() == &declaration.conflict_policy
            && metadata.durability() == declaration.durability
        {
            return Ok(metadata.id());
        }
        return Err(unsupported_runner_error(
            NodeId(0),
            None,
            format!(
                "relation {} already exists with different metadata",
                name.name().unwrap_or("<unnamed>")
            ),
        ));
    }

    let mut next_relation_id = next_generated_relation_id(kernel);
    loop {
        let Some(relation) = (next_relation_id < EPHEMERAL_HOST_IDENTITY_START)
            .then(|| Identity::new(next_relation_id))
            .flatten()
        else {
            return Err(unsupported_runner_error(
                NodeId(0),
                None,
                "generated relation identity exhausted",
            ));
        };
        next_relation_id += 1;
        let metadata = RelationMetadata::new(relation, name, declaration.arity)
            .with_conflict_policy(declaration.conflict_policy.clone())
            .with_durability(declaration.durability);
        match kernel.create_relation(metadata) {
            Ok(_) => return Ok(relation),
            Err(KernelError::RelationAlreadyExists(_)) => continue,
            Err(error) => return Err(CompileError::from(error).into()),
        }
    }
}

fn bootstrap_kernel() -> RelationKernel {
    bootstrap_kernel_with_provider(Arc::new(mica_relation_kernel::InMemoryCommitProvider::new()))
}

fn bootstrap_kernel_with_provider(
    provider: Arc<dyn mica_relation_kernel::CommitProvider>,
) -> RelationKernel {
    let kernel = RelationKernel::with_provider_and_computed_relations(
        provider,
        retrieval::default_computed_relations(),
    );
    kernel
        .create_relation(
            RelationMetadata::new(
                named_identity_relation(),
                Symbol::intern("NamedIdentity"),
                2,
            )
            .with_conflict_policy(ConflictPolicy::Functional {
                key_positions: vec![0],
            }),
        )
        .unwrap();
    for metadata in method_relation_metadata() {
        kernel.create_relation(metadata).unwrap();
    }
    for metadata in endpoint_relation_metadata() {
        kernel.create_relation(metadata).unwrap();
    }
    for metadata in system_relation_metadata() {
        kernel.create_relation(metadata).unwrap();
    }
    seed_primitive_prototype_identities(&kernel);
    kernel
}

fn seed_primitive_prototype_identities(kernel: &RelationKernel) {
    let mut tx = kernel.begin();
    for (name, identity) in PRIMITIVE_PROTOTYPES {
        tx.assert(
            named_identity_relation(),
            Tuple::from([
                Value::symbol(Symbol::intern(name)),
                Value::identity(*identity),
            ]),
        )
        .unwrap();
    }
    tx.commit().unwrap();
}

fn next_generated_method_identity_id(kernel: &RelationKernel) -> u64 {
    kernel
        .snapshot()
        .scan(named_identity_relation(), &[None, None])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tuple| tuple.values().get(1).and_then(Value::as_identity))
        .map(|identity| identity.raw())
        .filter(|raw| *raw >= GENERATED_METHOD_ID_START && *raw < GENERATED_IDENTITY_ID_START)
        .max()
        .and_then(|raw| raw.checked_add(1))
        .unwrap_or(GENERATED_METHOD_ID_START)
}

fn next_generated_identity_id(kernel: &RelationKernel) -> u64 {
    kernel
        .snapshot()
        .scan(named_identity_relation(), &[None, None])
        .unwrap_or_default()
        .into_iter()
        .filter_map(|tuple| tuple.values().get(1).and_then(Value::as_identity))
        .map(|identity| identity.raw())
        .filter(|raw| *raw >= GENERATED_IDENTITY_ID_START && *raw < GENERATED_RELATION_ID_START)
        .max()
        .and_then(|raw| raw.checked_add(1))
        .unwrap_or(GENERATED_IDENTITY_ID_START)
}

fn next_generated_relation_id(kernel: &RelationKernel) -> u64 {
    kernel
        .snapshot()
        .relation_metadata()
        .map(|metadata| metadata.id().raw())
        .filter(|raw| *raw >= GENERATED_RELATION_ID_START && *raw < EPHEMERAL_HOST_IDENTITY_START)
        .max()
        .and_then(|raw| raw.checked_add(1))
        .unwrap_or(GENERATED_RELATION_ID_START)
}

fn method_relations() -> MethodRelations {
    MethodRelations {
        dispatch: DispatchRelations {
            method_selector: method_selector_relation(),
            param: param_relation(),
            delegates: delegates_relation(),
        },
        method_program: method_program_relation(),
        program_bytes: program_bytes_relation(),
    }
}

fn invocation_program(
    selector: Symbol,
    roles: Vec<(Symbol, Value)>,
) -> Result<Program, RuntimeError> {
    invocation_program_with_delay(selector, roles, None)
}

fn invocation_program_with_delay(
    selector: Symbol,
    roles: Vec<(Symbol, Value)>,
    delay_millis: Option<u64>,
) -> Result<Program, RuntimeError> {
    let relations = method_relations();
    let mut instructions = Vec::new();
    if let Some(delay_millis) = delay_millis {
        instructions.push(Instruction::Suspend {
            kind: SuspendKind::TimedMillis(delay_millis),
        });
    }
    instructions.extend([
        Instruction::Dispatch {
            dst: Register(0),
            relations: relations.dispatch,
            program_relation: relations.method_program,
            program_bytes: relations.program_bytes,
            selector: Operand::Value(Value::symbol(selector)),
            roles: roles
                .into_iter()
                .map(|(role, value)| (Value::symbol(role), Operand::Value(value)))
                .collect(),
        },
        Instruction::Return {
            value: Operand::Register(Register(0)),
        },
    ]);
    Program::new(1, instructions)
}

fn positional_invocation_program_with_delay(
    selector: Symbol,
    args: Vec<Value>,
    delay_millis: Option<u64>,
) -> Result<Program, RuntimeError> {
    let relations = method_relations();
    let mut instructions = Vec::new();
    if let Some(delay_millis) = delay_millis {
        instructions.push(Instruction::Suspend {
            kind: SuspendKind::TimedMillis(delay_millis),
        });
    }
    instructions.extend([
        Instruction::PositionalDispatch {
            dst: Register(0),
            relations: relations.dispatch,
            program_relation: relations.method_program,
            program_bytes: relations.program_bytes,
            selector: Operand::Value(Value::symbol(selector)),
            args: args.into_iter().map(Operand::Value).collect(),
        },
        Instruction::Return {
            value: Operand::Register(Register(0)),
        },
    ]);
    Program::new(1, instructions)
}

fn spawn_invocation_program(
    selector: Symbol,
    target: SpawnTarget,
    principal: Option<Identity>,
    actor: Option<Identity>,
    endpoint: Identity,
    delay_millis: Option<u64>,
) -> Result<Program, RuntimeError> {
    match target {
        SpawnTarget::NamedRoles(roles) => invocation_program_with_delay(
            selector,
            invocation_roles(principal, actor, endpoint, roles),
            delay_millis,
        ),
        SpawnTarget::PositionalArgs(args) => {
            positional_invocation_program_with_delay(selector, args, delay_millis)
        }
    }
}

fn invocation_roles(
    principal: Option<Identity>,
    actor: Option<Identity>,
    endpoint: Identity,
    mut roles: Vec<(Symbol, Value)>,
) -> Vec<(Symbol, Value)> {
    push_context_role(&mut roles, "principal", principal);
    push_context_role(&mut roles, "actor", actor);
    push_required_context_role(&mut roles, "endpoint", endpoint);
    roles
}

fn runtime_context(
    principal: Option<Identity>,
    actor: Option<Identity>,
    endpoint: Identity,
) -> RuntimeContext {
    RuntimeContext::new(principal, actor, endpoint)
}

fn push_context_role(roles: &mut Vec<(Symbol, Value)>, name: &str, identity: Option<Identity>) {
    let Some(identity) = identity else {
        return;
    };
    let role = Symbol::intern(name);
    if roles.iter().any(|(existing, _)| *existing == role) {
        return;
    }
    roles.push((role, Value::identity(identity)));
}

fn push_required_context_role(roles: &mut Vec<(Symbol, Value)>, name: &str, identity: Identity) {
    let role = Symbol::intern(name);
    if roles.iter().any(|(existing, _)| *existing == role) {
        return;
    }
    roles.push((role, Value::identity(identity)));
}

fn method_relation_metadata() -> Vec<RelationMetadata> {
    vec![
        RelationMetadata::new(
            method_selector_relation(),
            Symbol::intern("MethodSelector"),
            2,
        )
        .with_index([1, 0]),
        RelationMetadata::new(param_relation(), Symbol::intern("Param"), 4).with_index([0, 1]),
        RelationMetadata::new(delegates_relation(), Symbol::intern("Delegates"), 3)
            .with_index([0, 2, 1]),
        RelationMetadata::new(
            method_program_relation(),
            Symbol::intern("MethodProgram"),
            2,
        )
        .with_index([0]),
        RelationMetadata::new(program_bytes_relation(), Symbol::intern("ProgramBytes"), 2)
            .with_index([0]),
        RelationMetadata::new(method_source_relation(), Symbol::intern("MethodSource"), 2)
            .with_conflict_policy(ConflictPolicy::Functional {
                key_positions: vec![0],
            }),
        RelationMetadata::new(
            source_owns_fact_relation(),
            Symbol::intern("SourceOwnsFact"),
            3,
        ),
        RelationMetadata::new(
            source_owns_rule_relation(),
            Symbol::intern("SourceOwnsRule"),
            2,
        ),
        RelationMetadata::new(
            source_owns_relation_relation(),
            Symbol::intern("SourceOwnsRelation"),
            2,
        ),
    ]
}

fn endpoint_relation_metadata() -> Vec<RelationMetadata> {
    vec![
        RelationMetadata::new(endpoint_relation(), Symbol::intern("Endpoint"), 1)
            .with_durability(RelationDurability::Volatile),
        RelationMetadata::new(
            endpoint_actor_relation(),
            Symbol::intern("EndpointActor"),
            2,
        )
        .with_index([1, 0])
        .with_index([0])
        .with_conflict_policy(ConflictPolicy::Functional {
            key_positions: vec![0],
        })
        .with_durability(RelationDurability::Volatile),
        RelationMetadata::new(
            endpoint_principal_relation(),
            Symbol::intern("EndpointPrincipal"),
            2,
        )
        .with_index([1, 0])
        .with_index([0])
        .with_conflict_policy(ConflictPolicy::Functional {
            key_positions: vec![0],
        })
        .with_durability(RelationDurability::Volatile),
        RelationMetadata::new(
            endpoint_protocol_relation(),
            Symbol::intern("EndpointProtocol"),
            2,
        )
        .with_index([0])
        .with_conflict_policy(ConflictPolicy::Functional {
            key_positions: vec![0],
        })
        .with_durability(RelationDurability::Volatile),
        RelationMetadata::new(endpoint_open_relation(), Symbol::intern("EndpointOpen"), 1)
            .with_durability(RelationDurability::Volatile),
    ]
}

fn system_relation_metadata() -> Vec<RelationMetadata> {
    vec![
        RelationMetadata::new(relation_relation(), Symbol::intern("Relation"), 1),
        RelationMetadata::new(relation_name_relation(), Symbol::intern("RelationName"), 2)
            .with_index([1, 0]),
        RelationMetadata::new(arity_relation(), Symbol::intern("Arity"), 2),
        RelationMetadata::new(
            relation_durability_relation(),
            Symbol::intern("RelationDurability"),
            2,
        ),
        RelationMetadata::new(rule_relation(), Symbol::intern("Rule"), 1),
        RelationMetadata::new(rule_head_relation(), Symbol::intern("RuleHead"), 2)
            .with_index([1, 0]),
        RelationMetadata::new(rule_source_relation(), Symbol::intern("RuleSource"), 2),
        RelationMetadata::new(active_rule_relation(), Symbol::intern("ActiveRule"), 2),
        RelationMetadata::new(argument_name_relation(), Symbol::intern("ArgumentName"), 3),
        RelationMetadata::new(
            conflict_policy_relation(),
            Symbol::intern("ConflictPolicy"),
            2,
        ),
        RelationMetadata::new(
            functional_key_relation(),
            Symbol::intern("FunctionalKey"),
            3,
        ),
        RelationMetadata::new(index_relation(), Symbol::intern("Index"), 2),
        RelationMetadata::new(
            index_position_relation(),
            Symbol::intern("IndexPosition"),
            3,
        ),
        RelationMetadata::new(
            index_storage_kind_relation(),
            Symbol::intern("IndexStorageKind"),
            2,
        ),
        RelationMetadata::new(subject_fact_relation(), Symbol::intern("SubjectFact"), 3)
            .with_index([0]),
        RelationMetadata::new(
            mentioned_fact_relation(),
            Symbol::intern("MentionedFact"),
            4,
        )
        .with_index([0]),
        RelationMetadata::new(
            extensional_mentioned_fact_relation(),
            Symbol::intern("ExtensionalMentionedFact"),
            4,
        )
        .with_index([0]),
    ]
}

fn default_builtins(embedding_provider: Arc<dyn embedding::EmbeddingProvider>) -> BuiltinRegistry {
    let registry = BuiltinRegistry::new()
        .with_builtin("emit", BuiltinResultKind::Dynamic, emit_builtin)
        .with_builtin(
            "log",
            BuiltinResultKind::Exact(ValueKind::Relation),
            log_builtin,
        )
        .with_builtin(
            "mailbox",
            BuiltinResultKind::Exact(ValueKind::List),
            mailbox_builtin,
        )
        .with_builtin(
            "mailbox_send",
            BuiltinResultKind::Dynamic,
            mailbox_send_builtin,
        )
        .with_builtin(
            "mailbox_close",
            BuiltinResultKind::Exact(ValueKind::Relation),
            mailbox_close_builtin,
        )
        .with_builtin(
            "subscribe_changes",
            BuiltinResultKind::Exact(ValueKind::Capability),
            subscribe_changes_builtin,
        )
        .with_builtin(
            "cancel_subscription",
            BuiltinResultKind::Exact(ValueKind::Relation),
            cancel_subscription_builtin,
        )
        .with_builtin(
            "make_relation",
            BuiltinResultKind::Exact(ValueKind::Identity),
            MakeRelationBuiltin::new(),
        )
        .with_builtin(
            "make_functional_relation",
            BuiltinResultKind::Exact(ValueKind::Identity),
            MakeFunctionalRelationBuiltin::new(),
        )
        .with_builtin(
            "make_identity",
            BuiltinResultKind::Exact(ValueKind::Identity),
            MakeIdentityBuiltin::new(),
        )
        .with_builtin(
            "rules",
            BuiltinResultKind::Exact(ValueKind::List),
            rules_builtin,
        )
        .with_builtin(
            "describe_rule",
            BuiltinResultKind::Exact(ValueKind::String),
            describe_rule_builtin,
        )
        .with_builtin(
            "disable_rule",
            BuiltinResultKind::Exact(ValueKind::Relation),
            disable_rule_builtin,
        )
        .with_builtin(
            "fileout",
            BuiltinResultKind::Exact(ValueKind::String),
            fileout_builtin,
        )
        .with_builtin(
            "fileout_rules",
            BuiltinResultKind::Exact(ValueKind::String),
            fileout_rules_builtin,
        )
        .with_builtin(
            "tasks",
            BuiltinResultKind::Exact(ValueKind::List),
            tasks_builtin,
        )
        .with_builtin("actor", BuiltinResultKind::Dynamic, actor_builtin)
        .with_builtin("principal", BuiltinResultKind::Dynamic, principal_builtin)
        .with_builtin(
            "endpoint",
            BuiltinResultKind::Exact(ValueKind::Identity),
            endpoint_builtin,
        )
        .with_builtin(
            "assume_actor",
            BuiltinResultKind::Exact(ValueKind::Identity),
            assume_actor_builtin,
        )
        .with_builtin(
            "destroy_identity",
            BuiltinResultKind::Exact(ValueKind::Int),
            destroy_identity_builtin,
        )
        .with_builtin(
            "frob",
            BuiltinResultKind::Exact(ValueKind::Frob),
            frob_builtin,
        )
        .with_builtin(
            "frob_delegate",
            BuiltinResultKind::Dynamic,
            frob_delegate_builtin,
        )
        .with_builtin("frob_value", BuiltinResultKind::Dynamic, frob_value_builtin)
        .with_builtin(
            "is_frob",
            BuiltinResultKind::Exact(ValueKind::Bool),
            is_frob_builtin,
        )
        .with_builtin(
            "to_literal",
            BuiltinResultKind::Exact(ValueKind::String),
            to_literal_builtin,
        )
        .with_builtin(
            "from_literal",
            BuiltinResultKind::Dynamic,
            from_literal_builtin,
        )
        .with_builtin(
            "to_symbol",
            BuiltinResultKind::Exact(ValueKind::Symbol),
            to_symbol_builtin,
        )
        .with_builtin(
            "map_pairs",
            BuiltinResultKind::Exact(ValueKind::List),
            map_pairs_builtin,
        )
        .with_builtin("index_or", BuiltinResultKind::Dynamic, index_or_builtin)
        .with_builtin(
            "project",
            BuiltinResultKind::Exact(ValueKind::Relation),
            project_builtin,
        )
        .with_builtin(
            "union",
            BuiltinResultKind::Exact(ValueKind::Relation),
            union_builtin,
        )
        .with_builtin(
            "difference",
            BuiltinResultKind::Exact(ValueKind::Relation),
            difference_builtin,
        )
        .with_builtin(
            "natural_join",
            BuiltinResultKind::Exact(ValueKind::Relation),
            natural_join_builtin,
        )
        .with_builtin(
            "json_encode",
            BuiltinResultKind::Exact(ValueKind::String),
            json_encode_builtin,
        )
        .with_builtin(
            "json_decode",
            BuiltinResultKind::Dynamic,
            json_decode_builtin,
        )
        .with_builtin(
            "dom_text",
            BuiltinResultKind::Exact(ValueKind::Map),
            dom_text_builtin,
        )
        .with_builtin(
            "dom_raw",
            BuiltinResultKind::Exact(ValueKind::Map),
            dom_raw_builtin,
        )
        .with_builtin(
            "dom_element",
            BuiltinResultKind::Exact(ValueKind::Map),
            dom_element_builtin,
        )
        .with_builtin(
            "dom_html",
            BuiltinResultKind::Exact(ValueKind::String),
            dom_html_builtin,
        )
        .with_builtin(
            "to_xml",
            BuiltinResultKind::Exact(ValueKind::String),
            to_xml_builtin,
        )
        .with_builtin("from_xml", BuiltinResultKind::Dynamic, from_xml_builtin)
        .with_builtin(
            "dom_diff",
            BuiltinResultKind::Exact(ValueKind::List),
            dom_diff_builtin,
        )
        .with_builtin(
            "dom_snapshot_payload",
            BuiltinResultKind::Exact(ValueKind::String),
            dom_snapshot_payload_builtin,
        )
        .with_builtin(
            "sync_signature",
            BuiltinResultKind::Exact(ValueKind::Int),
            sync_signature_builtin,
        );
    builtins::install_scalar_builtins(registry).with_builtin(
        "embed_text",
        BuiltinResultKind::Exact(ValueKind::List),
        embedding::EmbedTextBuiltin::new(embedding_provider),
    )
}

fn emit_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(RuntimeError::InvalidBuiltinCall {
            name: Symbol::intern("emit"),
            message: "emit expects target identity and value".to_owned(),
        });
    }
    let target_value = args[0].clone();
    let target = target_value
        .as_identity()
        .ok_or(RuntimeError::InvalidEffectTarget(target_value))?;
    let value = args[1].clone();
    context.emit(target, value.clone())?;
    Ok(value)
}

fn log_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 && args.len() != 2 {
        return Err(invalid_builtin_call(
            "log",
            "expected log(message) or log(:level, message)",
        ));
    }
    if !context.authority().can_effect() {
        return Err(RuntimeError::PermissionDenied {
            operation: "log",
            target: Value::symbol(Symbol::intern("log")),
        });
    }

    let (level, message) = if args.len() == 1 {
        ("info", builtin_string_arg("log", args, 0)?)
    } else {
        let level = builtin_symbol_arg("log", args, 0)?;
        let Some(level) = level.name() else {
            return Err(invalid_builtin_call("log", "log level must be named"));
        };
        (level, builtin_string_arg("log", args, 1)?)
    };
    let runtime_context = context.runtime_context();
    match level {
        "trace" => tracing::trace!(
            target: "mica_runtime::log",
            principal = ?runtime_context.principal(),
            actor = ?runtime_context.actor(),
            endpoint = ?runtime_context.endpoint(),
            message = %message,
            "mica log"
        ),
        "debug" => tracing::debug!(
            target: "mica_runtime::log",
            principal = ?runtime_context.principal(),
            actor = ?runtime_context.actor(),
            endpoint = ?runtime_context.endpoint(),
            message = %message,
            "mica log"
        ),
        "info" => tracing::info!(
            target: "mica_runtime::log",
            principal = ?runtime_context.principal(),
            actor = ?runtime_context.actor(),
            endpoint = ?runtime_context.endpoint(),
            message = %message,
            "mica log"
        ),
        "warn" => tracing::warn!(
            target: "mica_runtime::log",
            principal = ?runtime_context.principal(),
            actor = ?runtime_context.actor(),
            endpoint = ?runtime_context.endpoint(),
            message = %message,
            "mica log"
        ),
        "error" => tracing::error!(
            target: "mica_runtime::log",
            principal = ?runtime_context.principal(),
            actor = ?runtime_context.actor(),
            endpoint = ?runtime_context.endpoint(),
            message = %message,
            "mica log"
        ),
        _ => {
            return Err(invalid_builtin_call(
                "log",
                "log level must be one of :trace, :debug, :info, :warn, or :error",
            ));
        }
    }

    Ok(Value::nothing())
}

fn mailbox_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(invalid_builtin_call("mailbox", "expected mailbox()"));
    }
    let (receiver, sender) = context.create_mailbox()?;
    Ok(Value::list([receiver, sender]))
}

fn mailbox_send_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "mailbox_send",
            "expected mailbox_send(sender, value)",
        ));
    }
    let sender = args[0].clone();
    let value = args[1].clone();
    context.send_mailbox(sender, value.clone())?;
    Ok(value)
}

fn mailbox_close_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "mailbox_close",
            "expected mailbox_close(receiver)",
        ));
    }
    context.close_mailbox(&args[0])?;
    Ok(Value::nothing())
}

fn subscribe_changes_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if !(5..=7).contains(&args.len()) {
        return Err(invalid_builtin_call(
            "subscribe_changes",
            "expected subscribe_changes(sender, subject, relation, bindings, initial[, cursor[, queue_budget]])",
        ));
    }
    let subject_name = args[1]
        .as_symbol()
        .and_then(Symbol::name)
        .ok_or_else(|| invalid_builtin_call("subscribe_changes", "subject must be a symbol"))?;
    let bindings = args[3]
        .with_list(|values| {
            values
                .iter()
                .map(|value| (!value.is_empty_relation()).then(|| value.clone()))
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| invalid_builtin_call("subscribe_changes", "bindings must be a list"))?;
    let subject = match subject_name {
        "catalogue" => {
            if !args[2].is_empty_relation()
                || !bindings.is_empty()
                || !context.authority().is_root()
            {
                return Err(invalid_builtin_call(
                    "subscribe_changes",
                    "catalogue subscriptions require root authority, nothing for relation, and empty bindings",
                ));
            }
            SubscriptionSubject::Catalogue
        }
        "facts" | "relation" => {
            let relation = args[2]
                .as_identity()
                .or_else(|| {
                    args[2]
                        .as_symbol()
                        .and_then(|name| relation_named(context.kernel(), name))
                        .map(|(relation, _)| relation)
                })
                .ok_or_else(|| {
                    invalid_builtin_call(
                        "subscribe_changes",
                        "relation must be an identity or a known relation name",
                    )
                })?;
            let arity = context
                .kernel()
                .snapshot()
                .relation_metadata()
                .find(|metadata| metadata.id() == relation)
                .map(|metadata| metadata.arity() as usize)
                .ok_or(RuntimeError::Kernel(KernelError::UnknownRelation(relation)))?;
            if bindings.len() != arity {
                return Err(invalid_builtin_call(
                    "subscribe_changes",
                    format!("bindings must contain exactly {arity} entries"),
                ));
            }
            if subject_name == "facts" {
                if context
                    .kernel()
                    .snapshot()
                    .relation_capabilities(relation)?
                    .source
                    == RelationSource::Computed
                {
                    return Err(invalid_builtin_call(
                        "subscribe_changes",
                        "fact subscriptions require a stored relation",
                    ));
                }
                SubscriptionSubject::Facts { relation, bindings }
            } else {
                SubscriptionSubject::Relation { relation, bindings }
            }
        }
        _ => {
            return Err(invalid_builtin_call(
                "subscribe_changes",
                "subject must be :catalogue, :facts, or :relation",
            ));
        }
    };
    let initial_delivery = match args[4].as_symbol().and_then(Symbol::name) {
        Some("changes") => SubscriptionInitialDelivery::ChangesOnly,
        Some("snapshot") => SubscriptionInitialDelivery::SnapshotThenChanges,
        _ => {
            return Err(invalid_builtin_call(
                "subscribe_changes",
                "initial delivery must be :changes or :snapshot",
            ));
        }
    };
    let cursor = match args.get(5) {
        None => None,
        Some(value) if value.is_empty_relation() => None,
        Some(value) => Some(
            value
                .as_int()
                .and_then(|cursor| u64::try_from(cursor).ok())
                .ok_or_else(|| {
                    invalid_builtin_call(
                        "subscribe_changes",
                        "cursor must be a non-negative integer or nothing",
                    )
                })?,
        ),
    };
    let queue_budget = args
        .get(6)
        .map(|_| builtin_usize_arg("subscribe_changes", args, 6))
        .transpose()?
        .unwrap_or(64);
    context.subscribe_changes(SubscriptionRequest {
        sender: args[0].clone(),
        subject,
        initial_delivery,
        cursor,
        queue_budget,
    })
}

fn cancel_subscription_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "cancel_subscription",
            "expected cancel_subscription(subscription)",
        ));
    }
    context.cancel_subscription(args[0].clone())?;
    Ok(Value::nothing())
}

fn tasks_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(invalid_builtin_call("tasks", "expected tasks()"));
    }
    Ok(Value::list(context.task_snapshot().iter().cloned()))
}

fn frob_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "frob",
            "expected frob(delegate, value)",
        ));
    }
    let delegate = builtin_identity_arg("frob", args, 0)?;
    Ok(Value::frob(delegate, args[1].clone()))
}

fn frob_delegate_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "frob_delegate",
            "expected frob_delegate(value)",
        ));
    }
    Ok(args[0]
        .frob_delegate()
        .map(Value::identity)
        .unwrap_or_else(Value::nothing))
}

fn frob_value_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "frob_value",
            "expected frob_value(value)",
        ));
    }
    args[0]
        .frob_value()
        .cloned()
        .ok_or_else(|| invalid_builtin_call("frob_value", "expected frob argument"))
}

fn is_frob_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("is_frob", "expected is_frob(value)"));
    }
    Ok(Value::bool(args[0].frob_delegate().is_some()))
}

fn to_literal_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "to_literal",
            "expected to_literal(value)",
        ));
    }
    if !args[0].is_persistable() {
        return Err(invalid_builtin_call(
            "to_literal",
            "this value does not have a source literal",
        ));
    }

    let relation_names = relation_name_map(&context.kernel().snapshot());
    let identity_names = identity_name_map(context.tx())?;
    Ok(Value::string(source_literal(
        &args[0],
        &identity_names,
        &relation_names,
    )))
}

fn from_literal_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "from_literal",
            "expected from_literal(text)",
        ));
    }
    let Some(source) = args[0].with_str(str::to_owned) else {
        return Err(invalid_builtin_call(
            "from_literal",
            "expected from_literal(text)",
        ));
    };
    let ast = parse_ast(&source);
    if !ast.errors.is_empty() || ast.items.len() != 1 {
        return Ok(Value::nothing());
    }
    let Item::Expr { expr, .. } = &ast.items[0] else {
        return Ok(Value::nothing());
    };
    value_from_literal_expr(context, expr).map(|value| value.unwrap_or_else(Value::nothing))
}

fn to_symbol_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "to_symbol",
            "expected to_symbol(text)",
        ));
    }
    if args[0].as_symbol().is_some() {
        return Ok(args[0].clone());
    }
    let Some(name) = args[0].with_str(str::to_owned) else {
        return Err(invalid_builtin_call(
            "to_symbol",
            "expected string or symbol argument",
        ));
    };
    Ok(Value::symbol(Symbol::intern(&name)))
}

fn map_pairs_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("map_pairs", "expected map_pairs(map)"));
    }
    let Some(pairs) = args[0].with_map(|entries| {
        entries
            .iter()
            .map(|(key, value)| Value::list([key.clone(), value.clone()]))
            .collect::<Vec<_>>()
    }) else {
        return Err(invalid_builtin_call("map_pairs", "expected a map argument"));
    };
    Ok(Value::list(pairs))
}

fn index_or_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let [collection, index, default] = args else {
        return Err(invalid_builtin_call(
            "index_or",
            "expected index_or(collection, index, default)",
        ));
    };

    if collection.map_len().is_some() {
        return Ok(collection.map_get(index).unwrap_or_else(|| default.clone()));
    }

    if collection.list_len().is_some() {
        let Some(index) = index.as_int().and_then(|index| usize::try_from(index).ok()) else {
            return Err(invalid_builtin_call(
                "index_or",
                "list indexes must be non-negative integers",
            ));
        };
        return Ok(collection
            .list_get(index)
            .unwrap_or_else(|| default.clone()));
    }

    if collection.kind() == ValueKind::Relation {
        let Some(index) = index.as_int().and_then(|index| usize::try_from(index).ok()) else {
            return Err(invalid_builtin_call(
                "index_or",
                "relation indexes must be non-negative integers",
            ));
        };
        return Ok(collection
            .with_relation(|relation| {
                relation.rows().get(index).map(|row| {
                    Value::map(
                        relation
                            .heading()
                            .iter()
                            .zip(row.values())
                            .map(|(column, value)| (Value::symbol(*column), value.clone())),
                    )
                })
            })
            .flatten()
            .unwrap_or_else(|| default.clone()));
    }

    Err(invalid_builtin_call(
        "index_or",
        "expected a list, map, or relation collection",
    ))
}

fn project_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let Some((relation, columns)) = args.split_first() else {
        return Err(invalid_builtin_call(
            "project",
            "expected project(relation, :column, ...)",
        ));
    };
    let columns = columns
        .iter()
        .map(|column| {
            column
                .as_symbol()
                .ok_or_else(|| invalid_builtin_call("project", "expected symbol column arguments"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(result) =
        relation.with_relation(|relation| relation_algebra::project(relation, columns))
    else {
        return Err(invalid_builtin_call(
            "project",
            "expected relation argument",
        ));
    };
    result
        .map(Value::from)
        .map_err(|error| invalid_builtin_call("project", error.to_string()))
}

fn union_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    relation_binary_builtin(
        "union",
        "expected union(left, right)",
        args,
        relation_algebra::union,
    )
}

fn difference_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    relation_binary_builtin(
        "difference",
        "expected difference(left, right)",
        args,
        relation_algebra::difference,
    )
}

fn natural_join_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    relation_binary_builtin(
        "natural_join",
        "expected natural_join(left, right)",
        args,
        relation_algebra::natural_join,
    )
}

fn relation_binary_builtin(
    name: &str,
    expected: &str,
    args: &[Value],
    operation: fn(
        &RelationValue,
        &RelationValue,
    ) -> Result<RelationValue, relation_algebra::RelationAlgebraError>,
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(name, expected));
    }
    let Some(result) =
        args[0].with_relation(|left| args[1].with_relation(|right| operation(left, right)))
    else {
        return Err(invalid_builtin_call(name, "expected relation argument"));
    };
    let Some(result) = result else {
        return Err(invalid_builtin_call(name, "expected relation argument"));
    };
    result
        .map(Value::from)
        .map_err(|error| invalid_builtin_call(name, error.to_string()))
}

fn value_from_literal_expr(
    context: &mut BuiltinContext<'_, '_>,
    expr: &Expr,
) -> Result<Option<Value>, RuntimeError> {
    match expr {
        Expr::Literal { value, .. } => literal_value(value),
        Expr::Identity { name, .. } => identity_literal_value(context, name),
        Expr::Symbol { name, .. } => Ok(Some(Value::symbol(Symbol::intern(name)))),
        Expr::Frob {
            delegate, value, ..
        } => {
            let Some(delegate) = identity_literal_identity(context, delegate)? else {
                return Ok(None);
            };
            let Some(value) = value_from_literal_expr(context, value)? else {
                return Ok(None);
            };
            Ok(Some(Value::frob(delegate, value)))
        }
        Expr::List { items, .. } => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let CollectionItem::Expr(expr) = item else {
                    return Ok(None);
                };
                let Some(value) = value_from_literal_expr(context, expr)? else {
                    return Ok(None);
                };
                values.push(value);
            }
            Ok(Some(Value::list(values)))
        }
        Expr::Relation { heading, rows, .. } => {
            let mut tuples = Vec::with_capacity(rows.len());
            for row in rows {
                let mut values = Vec::with_capacity(row.len());
                for expr in row {
                    let Some(value) = value_from_literal_expr(context, expr)? else {
                        return Ok(None);
                    };
                    values.push(value);
                }
                tuples.push(mica_var::Tuple::new(values));
            }
            Value::relation(heading.iter().map(|column| Symbol::intern(column)), tuples)
                .map(Some)
                .map_err(|error| invalid_builtin_call("from_literal", error.to_string()))
        }
        Expr::Map { entries, .. } => {
            let mut values = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                let Some(key) = value_from_literal_expr(context, key)? else {
                    return Ok(None);
                };
                let Some(value) = value_from_literal_expr(context, value)? else {
                    return Ok(None);
                };
                values.push((key, value));
            }
            Ok(Some(Value::map(values)))
        }
        Expr::Unary {
            op: UnaryOp::Neg,
            expr,
            ..
        } => negated_literal_value(context, expr),
        Expr::Binary {
            op: BinaryOp::Range,
            left,
            right,
            ..
        } => {
            let Some(start) = value_from_literal_expr(context, left)? else {
                return Ok(None);
            };
            let end = if matches!(right.as_ref(), Expr::Hole { .. }) {
                None
            } else {
                Some(value_from_literal_expr(context, right)?.ok_or_else(|| {
                    invalid_builtin_call("from_literal", "invalid range endpoint literal")
                })?)
            };
            Ok(Some(Value::range(start, end)))
        }
        _ => Ok(None),
    }
}

fn literal_value(literal: &Literal) -> Result<Option<Value>, RuntimeError> {
    match literal {
        Literal::Int(value) => value
            .parse::<i64>()
            .ok()
            .and_then(|value| Value::int(value).ok())
            .map(Some)
            .ok_or_else(|| invalid_builtin_call("from_literal", "invalid integer literal")),
        Literal::Float(value) => value
            .parse::<f32>()
            .map_err(|_| invalid_builtin_call("from_literal", "invalid float literal"))
            .and_then(|value| {
                Value::float(value).map(Some).map_err(|_| {
                    invalid_builtin_call("from_literal", "float literal overflows binary32")
                })
            }),
        Literal::String(value) => Ok(Some(Value::string(value))),
        Literal::Bytes(value) => Ok(Some(Value::bytes(value))),
        Literal::Bool(value) => Ok(Some(Value::bool(*value))),
        Literal::ErrorCode(value) => Ok(Some(Value::error_code(Symbol::intern(value)))),
        Literal::Nothing => Ok(Some(Value::nothing())),
    }
}

fn negated_literal_value(
    context: &mut BuiltinContext<'_, '_>,
    expr: &Expr,
) -> Result<Option<Value>, RuntimeError> {
    let Some(value) = value_from_literal_expr(context, expr)? else {
        return Ok(None);
    };
    if let Some(value) = value.as_int() {
        let Some(value) = value.checked_neg() else {
            return Err(invalid_builtin_call(
                "from_literal",
                "invalid integer literal",
            ));
        };
        return Value::int(value)
            .map(Some)
            .map_err(|_| invalid_builtin_call("from_literal", "invalid integer literal"));
    }
    if let Some(value) = value.as_float() {
        return Value::float(-value)
            .map(Some)
            .map_err(|_| invalid_builtin_call("from_literal", "float literal overflows binary32"));
    }
    Ok(None)
}

fn identity_literal_value(
    context: &mut BuiltinContext<'_, '_>,
    name: &str,
) -> Result<Option<Value>, RuntimeError> {
    Ok(identity_literal_identity(context, name)?.map(Value::identity))
}

fn identity_literal_identity(
    context: &mut BuiltinContext<'_, '_>,
    name: &str,
) -> Result<Option<Identity>, RuntimeError> {
    if let Ok(raw) = name.parse::<u64>() {
        return Ok(Identity::new(raw));
    }
    identity_named(context, Symbol::intern(name))
}

fn json_encode_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "json_encode",
            "expected json_encode(value)",
        ));
    }
    let json = json_value(&args[0])?;
    serde_json::to_string(&json)
        .map(Value::string)
        .map_err(|error| invalid_builtin_call("json_encode", error.to_string()))
}

fn json_decode_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "json_decode",
            "expected json_decode(text)",
        ));
    }
    let text = builtin_string_arg("json_decode", args, 0)?;
    value_from_json_text(&text)
        .map_err(|error| invalid_builtin_call("json_decode", error.to_string()))
}

fn dom_text_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("dom_text", "expected dom_text(text)"));
    }
    let text = builtin_string_arg("dom_text", args, 0)?;
    Ok(dom_text_value(text))
}

fn dom_raw_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("dom_raw", "expected dom_raw(text)"));
    }
    let text = builtin_string_arg("dom_raw", args, 0)?;
    Ok(dom_raw_value(text))
}

fn dom_element_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 3 {
        return Err(invalid_builtin_call(
            "dom_element",
            "expected dom_element(tag, attrs, children)",
        ));
    }
    let tag = builtin_string_arg("dom_element", args, 0)?;
    if args[1].with_map(|_| ()).is_none() {
        return Err(invalid_builtin_call(
            "dom_element",
            "expected attribute map as second argument",
        ));
    }
    if args[2].with_list(|_| ()).is_none() {
        return Err(invalid_builtin_call(
            "dom_element",
            "expected child list as third argument",
        ));
    }
    Ok(dom_element_value(tag, args[1].clone(), args[2].clone()))
}

fn dom_html_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("dom_html", "expected dom_html(node)"));
    }
    let mut out = String::new();
    render_dom_html(&args[0], &mut out)?;
    Ok(Value::string(out))
}

fn to_xml_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("to_xml", "expected to_xml(node)"));
    }
    let mut out = String::new();
    render_dom_xml(&args[0], "to_xml", false, &mut out)?;
    Ok(Value::string(out))
}

fn from_xml_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("from_xml", "expected from_xml(text)"));
    }
    let xml = builtin_string_arg("from_xml", args, 0)?;
    parse_dom_xml(&xml)
}

fn dom_diff_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "dom_diff",
            "expected dom_diff(before, after)",
        ));
    }
    let before = DomNode::from_mica_value(&args[0])
        .map_err(|error| invalid_builtin_call("dom_diff", error))?;
    let after = DomNode::from_mica_value(&args[1])
        .map_err(|error| invalid_builtin_call("dom_diff", error))?;
    Ok(Value::list(
        diff_dom_nodes(&before, &after)
            .into_iter()
            .map(|patch| patch.to_mica_value()),
    ))
}

fn dom_snapshot_payload_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 3 {
        return Err(invalid_builtin_call(
            "dom_snapshot_payload",
            "expected dom_snapshot_payload(view, revision, root)",
        ));
    }
    let view = args[0]
        .as_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| {
            invalid_builtin_call(
                "dom_snapshot_payload",
                "view must be a non-negative integer",
            )
        })?;
    let revision = args[1]
        .as_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| {
            invalid_builtin_call(
                "dom_snapshot_payload",
                "revision must be a non-negative integer",
            )
        })?;
    let root = DomNode::from_mica_value(&args[2])
        .map_err(|error| invalid_builtin_call("dom_snapshot_payload", error))?;
    String::from_utf8(snapshot_payload_json(view, revision, &root))
        .map(Value::string)
        .map_err(|error| invalid_builtin_call("dom_snapshot_payload", error.to_string()))
}

fn sync_signature_builtin(
    _context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 2 {
        return Err(invalid_builtin_call(
            "sync_signature",
            "expected sync_signature(revision, payload)",
        ));
    }
    let revision = args[0]
        .as_int()
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| {
            invalid_builtin_call("sync_signature", "revision must be a non-negative integer")
        })?;
    let payload = builtin_string_arg("sync_signature", args, 1)?;
    let signature = sync_payload_signature(revision, payload.as_bytes());
    Value::int(i64::try_from(signature).expect("sync signatures fit in signed integers"))
        .map_err(|_| invalid_builtin_call("sync_signature", "signature is out of range"))
}

fn json_value(value: &Value) -> Result<serde_json::Value, RuntimeError> {
    crate::json::json_from_value(value)
        .map_err(|e| invalid_builtin_call("json_encode", e.to_string()))
}

fn dom_text_value(text: impl AsRef<str>) -> Value {
    Value::map([(Value::symbol(Symbol::intern("text")), Value::string(text))])
}

fn dom_raw_value(text: impl AsRef<str>) -> Value {
    Value::map([(Value::symbol(Symbol::intern("raw")), Value::string(text))])
}

fn dom_element_value(tag: impl AsRef<str>, attrs: Value, children: Value) -> Value {
    Value::map([
        (Value::symbol(Symbol::intern("attrs")), attrs),
        (Value::symbol(Symbol::intern("children")), children),
        (Value::symbol(Symbol::intern("tag")), Value::string(tag)),
    ])
}

fn render_dom_html(node: &Value, out: &mut String) -> Result<(), RuntimeError> {
    render_dom_xml(node, "dom_html", true, out)
}

fn render_dom_xml(
    node: &Value,
    builtin: &'static str,
    restrict_html_surface: bool,
    out: &mut String,
) -> Result<(), RuntimeError> {
    if let Some(result) = node.with_list(|nodes| {
        for child in nodes {
            render_dom_xml(child, builtin, restrict_html_surface, out)?;
        }
        Ok::<_, RuntimeError>(())
    }) {
        result?;
        return Ok(());
    }
    if let Some(text) = dom_text(node) {
        escape_html_text(&text, out);
        return Ok(());
    }
    if let Some(text) = node.with_str(str::to_owned) {
        escape_html_text(&text, out);
        return Ok(());
    }
    if let Some(raw) = dom_raw(node) {
        out.push_str(&raw);
        return Ok(());
    }

    let Some((tag, attrs, children)) = dom_element(node) else {
        return Err(invalid_builtin_call(
            builtin,
            "expected DOM text, element, or node list",
        ));
    };
    if restrict_html_surface {
        validate_dom_tag(&tag)?;
    } else {
        validate_xml_name(builtin, "tag", &tag)?;
    }
    out.push('<');
    out.push_str(&tag);
    let Some(entries) = attrs.with_map(|entries| {
        entries
            .iter()
            .map(|(key, value)| {
                Ok::<_, RuntimeError>((dom_attr_name(key)?, dom_attr_value(value)?))
            })
            .collect::<Result<Vec<_>, _>>()
    }) else {
        return Err(invalid_builtin_call(builtin, "element attrs must be a map"));
    };
    for (name, value) in entries? {
        if restrict_html_surface {
            validate_dom_attr(&name)?;
        } else {
            validate_xml_name(builtin, "attribute", &name)?;
        }
        out.push(' ');
        out.push_str(&name);
        out.push_str("=\"");
        escape_html_attr(&value, out);
        out.push('"');
    }
    out.push('>');
    for child in children {
        render_dom_xml(&child, builtin, restrict_html_surface, out)?;
    }
    out.push_str("</");
    out.push_str(&tag);
    out.push('>');
    Ok(())
}

fn dom_attr_name(value: &Value) -> Result<String, RuntimeError> {
    if let Some(text) = value.with_str(str::to_owned) {
        return Ok(text);
    }
    if let Some(symbol) = value.as_symbol()
        && let Some(name) = symbol.name()
    {
        return Ok(name.to_owned());
    }
    Err(invalid_builtin_call(
        "dom_html",
        "attribute names must be strings or named symbols",
    ))
}

fn dom_attr_value(value: &Value) -> Result<String, RuntimeError> {
    if let Some(text) = value.with_str(str::to_owned) {
        return Ok(text);
    }
    if let Some(boolean) = value.as_bool() {
        return Ok(boolean.to_string());
    }
    if let Some(integer) = value.as_int() {
        return Ok(integer.to_string());
    }
    Err(invalid_builtin_call(
        "dom_html",
        "attribute values must be strings, booleans, or integers",
    ))
}

fn validate_dom_tag(tag: &str) -> Result<(), RuntimeError> {
    is_supported_dom_tag(tag)
        .then_some(())
        .ok_or_else(|| invalid_builtin_call("dom_html", format!("unsupported DOM tag: {tag}")))
}

fn validate_dom_attr(name: &str) -> Result<(), RuntimeError> {
    is_supported_dom_attribute(name)
        .then_some(())
        .ok_or_else(|| {
            invalid_builtin_call("dom_html", format!("unsupported DOM attribute: {name}"))
        })
}

fn validate_xml_name(builtin: &'static str, kind: &str, name: &str) -> Result<(), RuntimeError> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(invalid_builtin_call(
            builtin,
            format!("{kind} name cannot be empty"),
        ));
    };
    if !is_xml_name_start(first) || !chars.all(is_xml_name_char) {
        return Err(invalid_builtin_call(
            builtin,
            format!("invalid XML {kind} name: {name}"),
        ));
    }
    Ok(())
}

fn is_xml_name_start(ch: char) -> bool {
    ch == '_' || ch == ':' || ch.is_ascii_alphabetic()
}

fn is_xml_name_char(ch: char) -> bool {
    is_xml_name_start(ch) || ch == '-' || ch == '.' || ch.is_ascii_digit()
}

struct DomXmlFrame {
    tag: String,
    attrs: Vec<(Value, Value)>,
    children: Vec<Value>,
}

fn parse_dom_xml(xml: &str) -> Result<Value, RuntimeError> {
    let reader = BufReader::new(xml.as_bytes());
    let parser = EventReader::new(reader);
    let mut stack: Vec<DomXmlFrame> = Vec::new();
    let mut roots = Vec::new();

    for event in parser {
        match event {
            Ok(XmlEvent::StartElement {
                name, attributes, ..
            }) => {
                let attrs = attributes
                    .into_iter()
                    .map(|attr| {
                        (
                            Value::string(attr.name.local_name),
                            Value::string(attr.value),
                        )
                    })
                    .collect::<Vec<_>>();
                stack.push(DomXmlFrame {
                    tag: name.local_name,
                    attrs,
                    children: Vec::new(),
                });
            }
            Ok(XmlEvent::EndElement { .. }) => {
                let Some(frame) = stack.pop() else {
                    return Err(invalid_builtin_call(
                        "from_xml",
                        "end tag without start tag",
                    ));
                };
                let node = dom_element_value(
                    frame.tag,
                    Value::map(frame.attrs),
                    Value::list(frame.children),
                );
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(node);
                } else {
                    roots.push(node);
                }
            }
            Ok(XmlEvent::Characters(text)) | Ok(XmlEvent::CData(text)) => {
                if text.trim().is_empty() {
                    continue;
                }
                let Some(frame) = stack.last_mut() else {
                    roots.push(dom_text_value(text));
                    continue;
                };
                frame.children.push(dom_text_value(text));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(invalid_builtin_call(
                    "from_xml",
                    format!("XML parse error: {error}"),
                ));
            }
        }
    }

    if !stack.is_empty() {
        return Err(invalid_builtin_call("from_xml", "unclosed XML element"));
    }
    match roots.len() {
        0 => Err(invalid_builtin_call(
            "from_xml",
            "XML did not contain a node",
        )),
        1 => Ok(roots.pop().unwrap()),
        _ => Ok(Value::list(roots)),
    }
}

fn escape_html_text(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn escape_html_attr(value: &str, out: &mut String) {
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn dom_text(value: &Value) -> Option<String> {
    value
        .map_get(&Value::symbol(Symbol::intern("text")))?
        .with_str(str::to_owned)
}

fn dom_raw(value: &Value) -> Option<String> {
    value
        .map_get(&Value::symbol(Symbol::intern("raw")))?
        .with_str(str::to_owned)
}

fn dom_element(value: &Value) -> Option<(String, Value, Vec<Value>)> {
    let tag_value = value.map_get(&Value::symbol(Symbol::intern("tag")))?;
    let tag = tag_value.with_str(str::to_owned)?;
    let attrs = value.map_get(&Value::symbol(Symbol::intern("attrs")))?;
    let children = value.map_get(&Value::symbol(Symbol::intern("children")))?;
    attrs.with_map(|_| ())?;
    let children = children.with_list(<[Value]>::to_vec)?;
    Some((tag, attrs, children))
}

fn actor_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    runtime_identity_builtin("actor", args, context.runtime_context().actor())
}

fn principal_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    runtime_identity_builtin("principal", args, context.runtime_context().principal())
}

fn endpoint_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(invalid_builtin_call("endpoint", "expected endpoint()"));
    }
    Ok(Value::identity(context.runtime_context().endpoint()))
}

fn assume_actor_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "assume_actor",
            "expected assume_actor(#actor)",
        ));
    }
    let actor = builtin_identity_arg("assume_actor", args, 0)?;
    require_endpoint_open(context, "assume_actor")?;
    require_actor_assumption(context, actor)?;
    replace_endpoint_identity_binding(context, endpoint_actor_relation(), actor)?;
    Ok(Value::identity(actor))
}

fn require_endpoint_open(
    context: &mut BuiltinContext<'_, '_>,
    operation: &'static str,
) -> Result<(), RuntimeError> {
    let endpoint = context.runtime_context().endpoint();
    let rows = context
        .tx()
        .scan(endpoint_open_relation(), &[Some(Value::identity(endpoint))])?;
    if rows.is_empty() {
        return Err(RuntimeError::PermissionDenied {
            operation,
            target: Value::identity(endpoint),
        });
    }
    Ok(())
}

fn require_actor_assumption(
    context: &mut BuiltinContext<'_, '_>,
    actor: Identity,
) -> Result<(), RuntimeError> {
    if context.authority().can_grant() {
        return Ok(());
    }
    let Some(principal) = context.runtime_context().principal() else {
        return Err(RuntimeError::PermissionDenied {
            operation: "assume_actor",
            target: Value::identity(actor),
        });
    };
    let Some(policy_relation) =
        runtime_policy_relation(context.kernel(), "session/CanAssumeActor", 2)?
    else {
        return Err(RuntimeError::PermissionDenied {
            operation: "assume_actor",
            target: Value::identity(actor),
        });
    };
    let rows = context.tx().scan(
        policy_relation,
        &[
            Some(Value::identity(principal)),
            Some(Value::identity(actor)),
        ],
    )?;
    if rows.is_empty() {
        return Err(RuntimeError::PermissionDenied {
            operation: "assume_actor",
            target: Value::identity(actor),
        });
    }
    Ok(())
}

fn replace_endpoint_identity_binding(
    context: &mut BuiltinContext<'_, '_>,
    relation: Identity,
    identity: Identity,
) -> Result<(), RuntimeError> {
    let endpoint = context.runtime_context().endpoint();
    context.tx().replace_functional(
        relation,
        Tuple::from([Value::identity(endpoint), Value::identity(identity)]),
    )?;
    Ok(())
}

fn destroy_identity_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "destroy_identity",
            "expected destroy_identity(#identity)",
        ));
    }
    require_admin_builtin(context, "destroy_identity")?;
    let identity = builtin_identity_arg("destroy_identity", args, 0)?;
    let value = Value::identity(identity);
    let facts = context.tx().subject_facts(&value)?;
    let mut retracted = BTreeSet::new();
    for fact in facts {
        if is_read_only_system_relation(fact.relation) {
            continue;
        }
        if retracted.insert((fact.relation, fact.tuple.clone())) {
            context.tx().retract(fact.relation, fact.tuple)?;
        }
    }
    let names = context
        .tx()
        .scan(named_identity_relation(), &[None, Some(value)])?;
    for name in names {
        if retracted.insert((named_identity_relation(), name.clone())) {
            context.tx().retract(named_identity_relation(), name)?;
        }
    }
    Value::int(retracted.len() as i64).map_err(|_| {
        invalid_builtin_call("destroy_identity", "destroyed fact count is out of range")
    })
}

fn runtime_identity_builtin(
    name: &'static str,
    args: &[Value],
    identity: Option<Identity>,
) -> Result<Value, RuntimeError> {
    if !args.is_empty() {
        return Err(invalid_builtin_call(name, format!("expected {name}()")));
    }
    Ok(identity.map(Value::identity).unwrap_or_else(Value::nothing))
}

fn unsupported_runner_error(
    node: NodeId,
    span: Option<mica_compiler::Span>,
    message: impl Into<String>,
) -> SourceTaskError {
    CompileError::Unsupported {
        node,
        span,
        message: message.into(),
    }
    .into()
}

fn rules_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("rules", "expected rules(:Relation)"));
    }
    let name = builtin_symbol_arg("rules", args, 0)?;
    let (relation, _) = relation_named(context.kernel(), name).ok_or_else(|| {
        raised_builtin_error(
            "E_INVARG",
            format!("unknown relation :{}", name.name().unwrap_or("<unnamed>")),
            Some(Value::symbol(name)),
        )
    })?;
    let rules = context
        .kernel()
        .snapshot()
        .rules()
        .iter()
        .filter(|rule| rule.active() && rule.rule().head_relation() == relation)
        .map(|rule| Value::identity(rule.id()))
        .collect::<Vec<_>>();
    Ok(Value::list(rules))
}

fn describe_rule_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "describe_rule",
            "expected describe_rule(#rule)",
        ));
    }
    let rule_id = builtin_identity_arg("describe_rule", args, 0)?;
    let source = context
        .kernel()
        .snapshot()
        .rules()
        .iter()
        .find(|rule| rule.id() == rule_id)
        .map(|rule| Value::string(rule.source()))
        .ok_or_else(|| {
            raised_builtin_error(
                "E_INVARG",
                "rule does not exist",
                Some(Value::identity(rule_id)),
            )
        })?;
    Ok(source)
}

fn fileout_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call("fileout", "expected fileout(:unit)"));
    }
    let unit = builtin_symbol_arg("fileout", args, 0)?;
    let source = fileout_unit_source(context.kernel(), unit)?;
    Ok(Value::string(source))
}

fn fileout_rules_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() > 1 {
        return Err(invalid_builtin_call(
            "fileout_rules",
            "expected fileout_rules() or fileout_rules(:Relation)",
        ));
    }
    let relation = if args.is_empty() {
        None
    } else {
        let name = builtin_symbol_arg("fileout_rules", args, 0)?;
        Some(
            relation_named(context.kernel(), name)
                .map(|(relation, _)| relation)
                .ok_or_else(|| {
                    raised_builtin_error(
                        "E_INVARG",
                        format!("unknown relation :{}", name.name().unwrap_or("<unnamed>")),
                        Some(Value::symbol(name)),
                    )
                })?,
        )
    };
    let snapshot = context.kernel().snapshot();
    let sources = snapshot
        .rules()
        .iter()
        .filter(|rule| rule.active())
        .filter(|rule| {
            relation
                .map(|relation| relation == rule.rule().head_relation())
                .unwrap_or(true)
        })
        .map(|rule| rule.source().trim().to_owned())
        .collect::<Vec<_>>();
    Ok(Value::string(sources.join("\n\n")))
}

fn disable_rule_builtin(
    context: &mut BuiltinContext<'_, '_>,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    if args.len() != 1 {
        return Err(invalid_builtin_call(
            "disable_rule",
            "expected disable_rule(#rule)",
        ));
    }
    let rule_id = builtin_identity_arg("disable_rule", args, 0)?;
    require_admin_builtin(context, "disable_rule")?;
    context.kernel().disable_rule(rule_id)?;
    Ok(Value::nothing())
}

fn require_admin_builtin(
    context: &BuiltinContext<'_, '_>,
    name: &'static str,
) -> Result<(), RuntimeError> {
    if context.authority().can_grant() {
        return Ok(());
    }
    Err(RuntimeError::PermissionDenied {
        operation: "grant",
        target: Value::symbol(Symbol::intern(name)),
    })
}

struct MakeRelationBuiltin {
    next_relation_id: AtomicU64,
}

struct MakeFunctionalRelationBuiltin {
    next_relation_id: AtomicU64,
}

struct MakeIdentityBuiltin {
    next_identity_id: AtomicU64,
}

impl MakeRelationBuiltin {
    fn new() -> Self {
        Self {
            next_relation_id: AtomicU64::new(GENERATED_RELATION_ID_START),
        }
    }
}

impl MakeFunctionalRelationBuiltin {
    fn new() -> Self {
        Self {
            next_relation_id: AtomicU64::new(GENERATED_RELATION_ID_START),
        }
    }
}

impl MakeIdentityBuiltin {
    fn new() -> Self {
        Self {
            next_identity_id: AtomicU64::new(GENERATED_IDENTITY_ID_START),
        }
    }
}

impl Builtin for MakeRelationBuiltin {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        if !matches!(args.len(), 2 | 3) {
            return Err(invalid_builtin_call(
                "make_relation",
                "expected make_relation(:Name, arity[, :durability])",
            ));
        }
        let name = builtin_symbol_arg("make_relation", args, 0)?;
        let arity = builtin_arity_arg("make_relation", args, 1)?;
        let durability = builtin_relation_durability_arg("make_relation", args, 2)?;
        require_admin_builtin(context, "make_relation")?;

        if let Some(metadata) = relation_metadata_named(context.kernel(), name) {
            if metadata.arity() == arity
                && metadata.conflict_policy() == &ConflictPolicy::Set
                && metadata.durability() == durability
            {
                return Ok(Value::identity(metadata.id()));
            }
            return Err(invalid_builtin_call(
                "make_relation",
                "relation name already exists with different metadata",
            ));
        }

        loop {
            let raw = self.next_relation_id.fetch_add(1, Ordering::Relaxed);
            let Some(relation) = (raw < EPHEMERAL_HOST_IDENTITY_START)
                .then(|| Identity::new(raw))
                .flatten()
            else {
                return Err(invalid_builtin_call(
                    "make_relation",
                    "generated relation identity exhausted",
                ));
            };
            let metadata = RelationMetadata::new(relation, name, arity).with_durability(durability);
            match context.kernel().create_relation(metadata) {
                Ok(_) => return Ok(Value::identity(relation)),
                Err(KernelError::RelationAlreadyExists(_)) => continue,
                Err(error) => return Err(RuntimeError::Kernel(error)),
            }
        }
    }
}

impl Builtin for MakeFunctionalRelationBuiltin {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        if !matches!(args.len(), 3 | 4) {
            return Err(invalid_builtin_call(
                "make_functional_relation",
                "expected make_functional_relation(:Name, arity, [key_positions][, :durability])",
            ));
        }
        let name = builtin_symbol_arg("make_functional_relation", args, 0)?;
        let arity = builtin_arity_arg("make_functional_relation", args, 1)?;
        let key_positions = builtin_key_positions_arg("make_functional_relation", args, 2, arity)?;
        let durability = builtin_relation_durability_arg("make_functional_relation", args, 3)?;
        require_admin_builtin(context, "make_functional_relation")?;

        if let Some(metadata) = relation_metadata_named(context.kernel(), name) {
            if metadata.arity() == arity
                && metadata.conflict_policy()
                    == &(ConflictPolicy::Functional {
                        key_positions: key_positions.clone(),
                    })
                && metadata.durability() == durability
            {
                return Ok(Value::identity(metadata.id()));
            }
            return Err(invalid_builtin_call(
                "make_functional_relation",
                "relation name already exists with different metadata",
            ));
        }

        loop {
            let raw = self.next_relation_id.fetch_add(1, Ordering::Relaxed);
            let Some(relation) = (raw < EPHEMERAL_HOST_IDENTITY_START)
                .then(|| Identity::new(raw))
                .flatten()
            else {
                return Err(invalid_builtin_call(
                    "make_functional_relation",
                    "generated relation identity exhausted",
                ));
            };
            let metadata = RelationMetadata::new(relation, name, arity)
                .with_conflict_policy(ConflictPolicy::Functional {
                    key_positions: key_positions.clone(),
                })
                .with_durability(durability);
            match context.kernel().create_relation(metadata) {
                Ok(_) => return Ok(Value::identity(relation)),
                Err(KernelError::RelationAlreadyExists(_)) => continue,
                Err(error) => return Err(RuntimeError::Kernel(error)),
            }
        }
    }
}

impl Builtin for MakeIdentityBuiltin {
    fn call(
        &self,
        context: &mut BuiltinContext<'_, '_>,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        if args.len() != 1 {
            return Err(invalid_builtin_call(
                "make_identity",
                "expected make_identity(:name)",
            ));
        }
        let name = builtin_symbol_arg("make_identity", args, 0)?;
        require_admin_builtin(context, "make_identity")?;

        if let Some(identity) = identity_named(context, name)? {
            return Ok(Value::identity(identity));
        }

        let identity = loop {
            let raw = self.next_identity_id.fetch_add(1, Ordering::Relaxed);
            let Some(identity) = (raw < GENERATED_RELATION_ID_START)
                .then(|| Identity::new(raw))
                .flatten()
            else {
                return Err(invalid_builtin_call(
                    "make_identity",
                    "generated identity exhausted",
                ));
            };
            if context
                .tx()
                .scan(
                    named_identity_relation(),
                    &[None, Some(Value::identity(identity))],
                )?
                .is_empty()
            {
                break identity;
            }
        };
        context.tx().replace_functional(
            named_identity_relation(),
            Tuple::from([Value::symbol(name), Value::identity(identity)]),
        )?;
        Ok(Value::identity(identity))
    }
}

fn relation_named(kernel: &RelationKernel, name: Symbol) -> Option<(Identity, u16)> {
    let snapshot = kernel.snapshot();
    snapshot
        .relation_metadata()
        .find(|metadata| metadata.name() == name)
        .map(|metadata| (metadata.id(), metadata.arity()))
}

fn relation_name_index(kernel: &RelationKernel) -> BTreeMap<Symbol, (Identity, u16)> {
    let snapshot = kernel.snapshot();
    snapshot
        .relation_metadata()
        .map(|metadata| (metadata.name(), (metadata.id(), metadata.arity())))
        .collect()
}

fn relation_metadata_named(kernel: &RelationKernel, name: Symbol) -> Option<RelationMetadata> {
    let snapshot = kernel.snapshot();
    snapshot
        .relation_metadata()
        .find(|metadata| metadata.name() == name)
        .cloned()
}

fn volatile_tuple_relations_required(
    kernel: &RelationKernel,
    tuples: Vec<(Symbol, Tuple)>,
) -> Result<Vec<(Identity, Tuple)>, SourceTaskError> {
    let snapshot = kernel.snapshot();
    tuples
        .into_iter()
        .map(|(relation, tuple)| {
            let metadata = snapshot.relation_metadata_named(relation).ok_or_else(|| {
                unsupported_runner_error(
                    NodeId(0),
                    None,
                    format!(
                        "unknown relation {}",
                        relation.name().unwrap_or("<unnamed>")
                    ),
                )
            })?;
            ensure_tuple_arity(metadata.id(), metadata.arity(), tuple.arity())?;
            if metadata.durability() != RelationDurability::Volatile {
                return Err(unsupported_runner_error(
                    NodeId(0),
                    None,
                    format!(
                        "relation {} is not volatile",
                        relation.name().unwrap_or("<unnamed>")
                    ),
                ));
            }
            Ok((metadata.id(), tuple))
        })
        .collect()
}

fn assert_volatile_tuples_named_in(
    kernel: &RelationKernel,
    tuples: Vec<(Symbol, Tuple)>,
) -> Result<usize, SourceTaskError> {
    let tuples = volatile_tuple_relations_required(kernel, tuples)?;
    if tuples.is_empty() {
        return Ok(0);
    }
    let mut transaction = kernel.begin();
    for (relation, tuple) in tuples {
        transaction
            .assert(relation, tuple)
            .map_err(CompileError::from)?;
    }
    let result = transaction.commit().map_err(CompileError::from)?;
    Ok(result.commit().changes().len())
}

fn retract_volatile_tuples_named_in(
    kernel: &RelationKernel,
    tuples: Vec<(Symbol, Tuple)>,
) -> Result<usize, SourceTaskError> {
    let tuples = volatile_tuple_relations_required(kernel, tuples)?;
    if tuples.is_empty() {
        return Ok(0);
    }
    let mut transaction = kernel.begin();
    for (relation, tuple) in tuples {
        transaction
            .retract(relation, tuple)
            .map_err(CompileError::from)?;
    }
    let result = transaction.commit().map_err(CompileError::from)?;
    Ok(result.commit().changes().len())
}

fn replace_volatile_tuples_named_in(
    kernel: &RelationKernel,
    retract: Vec<(Symbol, Tuple)>,
    assert: Vec<(Symbol, Tuple)>,
) -> Result<usize, SourceTaskError> {
    let retract = volatile_tuple_relations_required(kernel, retract)?;
    let assert = volatile_tuple_relations_required(kernel, assert)?;
    if retract.is_empty() && assert.is_empty() {
        return Ok(0);
    }
    let mut transaction = kernel.begin();
    for (relation, tuple) in retract {
        transaction
            .retract(relation, tuple)
            .map_err(CompileError::from)?;
    }
    for (relation, tuple) in assert {
        transaction
            .assert(relation, tuple)
            .map_err(CompileError::from)?;
    }
    let result = transaction.commit().map_err(CompileError::from)?;
    Ok(result.commit().changes().len())
}

fn ensure_tuple_arity(
    relation: Identity,
    expected: u16,
    actual: usize,
) -> Result<(), SourceTaskError> {
    if actual == expected as usize {
        return Ok(());
    }
    Err(SourceTaskError::from(TaskManagerError::from(
        TaskError::from(KernelError::ArityMismatch {
            relation,
            expected,
            actual,
        }),
    )))
}

fn authority_for_actor(
    kernel: &RelationKernel,
    actor: Identity,
) -> Result<AuthorityContext, SourceTaskError> {
    let mut authority = AuthorityContext::empty();
    let relation_names = relation_name_index(kernel);
    for policy_name in ["CanRead", "GrantRead"] {
        mint_relation_grants(
            kernel,
            actor,
            policy_name,
            CapabilityOp::Read,
            &relation_names,
            &mut authority,
        )?;
    }
    mint_role_relation_grants(
        kernel,
        actor,
        "RoleCanRead",
        CapabilityOp::Read,
        &relation_names,
        &mut authority,
    )?;
    for policy_name in ["CanWrite", "GrantWrite"] {
        mint_relation_grants(
            kernel,
            actor,
            policy_name,
            CapabilityOp::Write,
            &relation_names,
            &mut authority,
        )?;
    }
    mint_role_relation_grants(
        kernel,
        actor,
        "RoleCanWrite",
        CapabilityOp::Write,
        &relation_names,
        &mut authority,
    )?;
    for policy_name in ["CanInvoke", "GrantInvoke"] {
        mint_invoke_grants(kernel, actor, policy_name, &relation_names, &mut authority)?;
    }
    mint_role_invoke_grants(
        kernel,
        actor,
        "RoleCanInvoke",
        &relation_names,
        &mut authority,
    )?;
    for policy_name in ["CanEffect", "GrantEffect"] {
        mint_effect_grants(kernel, actor, policy_name, &relation_names, &mut authority)?;
    }
    mint_role_effect_grants(
        kernel,
        actor,
        "RoleCanEffect",
        &relation_names,
        &mut authority,
    )?;
    Ok(authority)
}

fn authority_for_runtime_context(
    kernel: &RelationKernel,
    runtime_context: RuntimeContext,
) -> Result<AuthorityContext, SourceTaskError> {
    match (runtime_context.actor(), runtime_context.principal()) {
        (Some(actor), _) => authority_for_actor(kernel, actor),
        (None, Some(principal)) => authority_for_actor(kernel, principal),
        (None, None) => Ok(AuthorityContext::empty()),
    }
}

fn read_only_authority_for_actor(
    kernel: &RelationKernel,
    actor: Identity,
) -> Result<AuthorityContext, SourceTaskError> {
    let mut authority = AuthorityContext::empty();
    let relation_names = relation_name_index(kernel);
    for policy_name in ["CanRead", "GrantRead"] {
        mint_relation_grants(
            kernel,
            actor,
            policy_name,
            CapabilityOp::Read,
            &relation_names,
            &mut authority,
        )?;
    }
    mint_role_relation_grants(
        kernel,
        actor,
        "RoleCanRead",
        CapabilityOp::Read,
        &relation_names,
        &mut authority,
    )?;
    Ok(authority)
}

fn read_only_authority_for_runtime_context(
    kernel: &RelationKernel,
    runtime_context: RuntimeContext,
) -> Result<AuthorityContext, SourceTaskError> {
    match (runtime_context.actor(), runtime_context.principal()) {
        (Some(actor), _) => read_only_authority_for_actor(kernel, actor),
        (None, Some(principal)) => read_only_authority_for_actor(kernel, principal),
        (None, None) => Ok(AuthorityContext::empty()),
    }
}

fn mint_relation_grants(
    kernel: &RelationKernel,
    actor: Identity,
    policy_name: &str,
    op: CapabilityOp,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    authority: &mut AuthorityContext,
) -> Result<(), SourceTaskError> {
    let Some(policy_relation) = policy_relation_from_index(relation_names, policy_name, 2)? else {
        return Ok(());
    };
    let snapshot = kernel.snapshot();
    let tuples = snapshot
        .scan(policy_relation, &[Some(Value::identity(actor)), None])
        .map_err(CompileError::from)?;
    for tuple in tuples {
        let Some(relation_name) = tuple.values().get(1).and_then(Value::as_symbol) else {
            return Err(invalid_policy_fact(
                policy_name,
                "expected relation name symbol",
            ));
        };
        if let Some((relation, _)) = relation_names.get(&relation_name).copied() {
            authority.mint(CapabilityGrant::relation(op, relation));
        }
    }
    Ok(())
}

fn role_identities(
    kernel: &RelationKernel,
    actor: Identity,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
) -> Result<Vec<Identity>, SourceTaskError> {
    let mut roles = BTreeSet::from([actor]);
    let Some(delegates) = policy_relation_from_index(relation_names, "Delegates", 3)? else {
        return Ok(roles.into_iter().collect());
    };
    let snapshot = kernel.snapshot();
    for tuple in snapshot
        .scan(delegates, &[Some(Value::identity(actor)), None, None])
        .map_err(CompileError::from)?
    {
        if let Some(role) = tuple.values().get(1).and_then(Value::as_identity) {
            roles.insert(role);
        }
    }
    Ok(roles.into_iter().collect())
}

fn mint_role_relation_grants(
    kernel: &RelationKernel,
    actor: Identity,
    policy_name: &str,
    op: CapabilityOp,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    authority: &mut AuthorityContext,
) -> Result<(), SourceTaskError> {
    let Some(policy_relation) = policy_relation_from_index(relation_names, policy_name, 2)? else {
        return Ok(());
    };
    let roles = role_identities(kernel, actor, relation_names)?;
    let snapshot = kernel.snapshot();
    for role in roles {
        let tuples = snapshot
            .scan(policy_relation, &[Some(Value::identity(role)), None])
            .map_err(CompileError::from)?;
        for tuple in tuples {
            let Some(relation_name) = tuple.values().get(1).and_then(Value::as_symbol) else {
                return Err(invalid_policy_fact(
                    policy_name,
                    "expected relation name symbol",
                ));
            };
            if let Some((relation, _)) = relation_names.get(&relation_name).copied() {
                authority.mint(CapabilityGrant::relation(op, relation));
            }
        }
    }
    Ok(())
}

fn mint_invoke_grants(
    kernel: &RelationKernel,
    actor: Identity,
    policy_name: &str,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    authority: &mut AuthorityContext,
) -> Result<(), SourceTaskError> {
    let Some(policy_relation) = policy_relation_from_index(relation_names, policy_name, 2)? else {
        return Ok(());
    };
    let snapshot = kernel.snapshot();
    let tuples = snapshot
        .scan(policy_relation, &[Some(Value::identity(actor)), None])
        .map_err(CompileError::from)?;
    for tuple in tuples {
        let Some(selector) = tuple.values().get(1).and_then(Value::as_symbol) else {
            return Err(invalid_policy_fact(
                policy_name,
                "expected selector name symbol",
            ));
        };
        for method in snapshot
            .scan(
                method_selector_relation(),
                &[None, Some(Value::symbol(selector))],
            )
            .map_err(CompileError::from)?
        {
            if let Some(method) = method.values().first() {
                authority.mint(CapabilityGrant::method(method.clone()));
            }
        }
    }
    Ok(())
}

fn mint_role_invoke_grants(
    kernel: &RelationKernel,
    actor: Identity,
    policy_name: &str,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    authority: &mut AuthorityContext,
) -> Result<(), SourceTaskError> {
    let Some(policy_relation) = policy_relation_from_index(relation_names, policy_name, 2)? else {
        return Ok(());
    };
    let roles = role_identities(kernel, actor, relation_names)?;
    let snapshot = kernel.snapshot();
    for role in roles {
        let tuples = snapshot
            .scan(policy_relation, &[Some(Value::identity(role)), None])
            .map_err(CompileError::from)?;
        for tuple in tuples {
            let Some(selector) = tuple.values().get(1).and_then(Value::as_symbol) else {
                return Err(invalid_policy_fact(
                    policy_name,
                    "expected selector name symbol",
                ));
            };
            for method in snapshot
                .scan(
                    method_selector_relation(),
                    &[None, Some(Value::symbol(selector))],
                )
                .map_err(CompileError::from)?
            {
                if let Some(method) = method.values().first() {
                    authority.mint(CapabilityGrant::method(method.clone()));
                }
            }
        }
    }
    Ok(())
}

fn mint_effect_grants(
    kernel: &RelationKernel,
    actor: Identity,
    policy_name: &str,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    authority: &mut AuthorityContext,
) -> Result<(), SourceTaskError> {
    let Some(policy_relation) = policy_relation_from_index(relation_names, policy_name, 1)? else {
        return Ok(());
    };
    let snapshot = kernel.snapshot();
    if !snapshot
        .scan(policy_relation, &[Some(Value::identity(actor))])
        .map_err(CompileError::from)?
        .is_empty()
    {
        authority.mint(CapabilityGrant::new(
            [CapabilityOp::Effect],
            CapabilityScope::All,
        ));
    }
    Ok(())
}

fn mint_role_effect_grants(
    kernel: &RelationKernel,
    actor: Identity,
    policy_name: &str,
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    authority: &mut AuthorityContext,
) -> Result<(), SourceTaskError> {
    let Some(policy_relation) = policy_relation_from_index(relation_names, policy_name, 1)? else {
        return Ok(());
    };
    let roles = role_identities(kernel, actor, relation_names)?;
    let snapshot = kernel.snapshot();
    for role in roles {
        if !snapshot
            .scan(policy_relation, &[Some(Value::identity(role))])
            .map_err(CompileError::from)?
            .is_empty()
        {
            authority.mint(CapabilityGrant::new(
                [CapabilityOp::Effect],
                CapabilityScope::All,
            ));
        }
    }
    Ok(())
}

fn policy_relation_from_index(
    relation_names: &BTreeMap<Symbol, (Identity, u16)>,
    name: &str,
    expected_arity: u16,
) -> Result<Option<Identity>, SourceTaskError> {
    let Some((relation, arity)) = relation_names.get(&Symbol::intern(name)).copied() else {
        return Ok(None);
    };
    if arity != expected_arity {
        return Err(unsupported_runner_error(
            NodeId(0),
            None,
            format!("policy relation {name} has arity {arity}, expected {expected_arity}"),
        ));
    }
    Ok(Some(relation))
}

fn runtime_policy_relation(
    kernel: &RelationKernel,
    name: &str,
    expected_arity: u16,
) -> Result<Option<Identity>, RuntimeError> {
    let Some((relation, arity)) = relation_named(kernel, Symbol::intern(name)) else {
        return Ok(None);
    };
    if arity != expected_arity {
        return Err(RuntimeError::InvalidBuiltinCall {
            name: Symbol::intern(name),
            message: format!("policy relation {name} has arity {arity}, expected {expected_arity}"),
        });
    }
    Ok(Some(relation))
}

fn invalid_policy_fact(relation: &str, message: &str) -> SourceTaskError {
    unsupported_runner_error(
        NodeId(0),
        None,
        format!("{relation} policy fact is invalid: {message}"),
    )
}

fn identity_named_in_kernel(
    kernel: &RelationKernel,
    name: Symbol,
) -> Result<Option<Identity>, SourceTaskError> {
    let snapshot = kernel.snapshot();
    let tuples = snapshot
        .scan(
            named_identity_relation(),
            &[Some(Value::symbol(name)), None],
        )
        .map_err(CompileError::from)?;
    Ok(tuples
        .first()
        .and_then(|tuple| tuple.values().get(1))
        .and_then(Value::as_identity))
}

fn identity_named(
    context: &mut BuiltinContext<'_, '_>,
    name: Symbol,
) -> Result<Option<Identity>, RuntimeError> {
    let tuples = context.tx().scan(
        named_identity_relation(),
        &[Some(Value::symbol(name)), None],
    )?;
    Ok(tuples
        .first()
        .and_then(|tuple| tuple.values().get(1))
        .and_then(Value::as_identity))
}

fn identity_named_in_tx(
    tx: &mica_relation_kernel::Transaction<'_>,
    name: Symbol,
) -> Result<Option<Identity>, KernelError> {
    let tuples = tx.scan(
        named_identity_relation(),
        &[Some(Value::symbol(name)), None],
    )?;
    Ok(tuples
        .first()
        .and_then(|tuple| tuple.values().get(1))
        .and_then(Value::as_identity))
}

fn named_identity_relation() -> Identity {
    Identity::new(NAMED_IDENTITY_RELATION_ID).unwrap()
}

fn method_selector_relation() -> Identity {
    Identity::new(METHOD_SELECTOR_RELATION_ID).unwrap()
}

fn param_relation() -> Identity {
    Identity::new(PARAM_RELATION_ID).unwrap()
}

fn delegates_relation() -> Identity {
    Identity::new(DELEGATES_RELATION_ID).unwrap()
}

fn method_program_relation() -> Identity {
    Identity::new(METHOD_PROGRAM_RELATION_ID).unwrap()
}

fn program_bytes_relation() -> Identity {
    Identity::new(PROGRAM_BYTES_RELATION_ID).unwrap()
}

fn method_source_relation() -> Identity {
    Identity::new(METHOD_SOURCE_RELATION_ID).unwrap()
}

fn source_owns_fact_relation() -> Identity {
    Identity::new(SOURCE_OWNS_FACT_RELATION_ID).unwrap()
}

fn source_owns_rule_relation() -> Identity {
    Identity::new(SOURCE_OWNS_RULE_RELATION_ID).unwrap()
}

fn source_owns_relation_relation() -> Identity {
    Identity::new(SOURCE_OWNS_RELATION_RELATION_ID).unwrap()
}

fn endpoint_relation() -> Identity {
    Identity::new(ENDPOINT_RELATION_ID).unwrap()
}

fn endpoint_actor_relation() -> Identity {
    Identity::new(ENDPOINT_ACTOR_RELATION_ID).unwrap()
}

fn endpoint_principal_relation() -> Identity {
    Identity::new(ENDPOINT_PRINCIPAL_RELATION_ID).unwrap()
}

fn endpoint_protocol_relation() -> Identity {
    Identity::new(ENDPOINT_PROTOCOL_RELATION_ID).unwrap()
}

fn endpoint_open_relation() -> Identity {
    Identity::new(ENDPOINT_OPEN_RELATION_ID).unwrap()
}

fn relation_relation() -> Identity {
    Identity::new(RELATION_RELATION_ID).unwrap()
}

fn relation_name_relation() -> Identity {
    Identity::new(RELATION_NAME_RELATION_ID).unwrap()
}

fn arity_relation() -> Identity {
    Identity::new(ARITY_RELATION_ID).unwrap()
}

fn relation_durability_relation() -> Identity {
    Identity::new(RELATION_DURABILITY_RELATION_ID).unwrap()
}

fn rule_relation() -> Identity {
    Identity::new(RULE_RELATION_ID).unwrap()
}

fn rule_head_relation() -> Identity {
    Identity::new(RULE_HEAD_RELATION_ID).unwrap()
}

fn rule_source_relation() -> Identity {
    Identity::new(RULE_SOURCE_RELATION_ID).unwrap()
}

fn active_rule_relation() -> Identity {
    Identity::new(ACTIVE_RULE_RELATION_ID).unwrap()
}

fn argument_name_relation() -> Identity {
    Identity::new(ARGUMENT_NAME_RELATION_ID).unwrap()
}

fn conflict_policy_relation() -> Identity {
    Identity::new(CONFLICT_POLICY_RELATION_ID).unwrap()
}

fn functional_key_relation() -> Identity {
    Identity::new(FUNCTIONAL_KEY_RELATION_ID).unwrap()
}

fn index_relation() -> Identity {
    Identity::new(INDEX_RELATION_ID).unwrap()
}

fn index_position_relation() -> Identity {
    Identity::new(INDEX_POSITION_RELATION_ID).unwrap()
}

fn index_storage_kind_relation() -> Identity {
    Identity::new(INDEX_STORAGE_KIND_RELATION_ID).unwrap()
}

fn subject_fact_relation() -> Identity {
    Identity::new(SUBJECT_FACT_RELATION_ID).unwrap()
}

fn mentioned_fact_relation() -> Identity {
    Identity::new(MENTIONED_FACT_RELATION_ID).unwrap()
}

fn extensional_mentioned_fact_relation() -> Identity {
    Identity::new(EXTENSIONAL_MENTIONED_FACT_RELATION_ID).unwrap()
}

fn item_id(item: &HirItem) -> mica_compiler::NodeId {
    match item {
        HirItem::Expr { id, .. }
        | HirItem::RelationRule { id, .. }
        | HirItem::Method { id, .. } => *id,
    }
}

fn reject_semantic_parse_or_diagnostic(
    semantic: &mica_compiler::SemanticProgram,
) -> Result<(), CompileError> {
    if !semantic.parse_errors.is_empty() {
        return Err(CompileError::ParseErrors {
            errors: semantic.parse_errors.clone(),
        });
    }
    if let Some(diagnostic) = semantic.diagnostics.first() {
        return Err(CompileError::SemanticDiagnostic {
            diagnostic: diagnostic.clone(),
        });
    }
    Ok(())
}

fn validate_read_only_source_query(
    semantic: &mica_compiler::SemanticProgram,
) -> Result<(), CompileError> {
    for item in &semantic.hir.items {
        validate_read_only_item(semantic, item)?;
    }
    Ok(())
}

fn validate_read_only_item(
    semantic: &mica_compiler::SemanticProgram,
    item: &HirItem,
) -> Result<(), CompileError> {
    match item {
        HirItem::Expr { expr, .. } => validate_read_only_expr(semantic, expr),
        HirItem::RelationRule { .. } => Err(read_only_query_rejection(
            semantic,
            item_id(item),
            "read-only query cannot install relation rules",
        )),
        HirItem::Method { .. } => Err(read_only_query_rejection(
            semantic,
            item_id(item),
            "read-only query cannot install methods",
        )),
    }
}

fn validate_read_only_items(
    semantic: &mica_compiler::SemanticProgram,
    items: &[HirItem],
) -> Result<(), CompileError> {
    for item in items {
        validate_read_only_item(semantic, item)?;
    }
    Ok(())
}

fn validate_read_only_expr(
    semantic: &mica_compiler::SemanticProgram,
    expr: &HirExpr,
) -> Result<(), CompileError> {
    match expr {
        HirExpr::Literal { .. }
        | HirExpr::LocalRef { .. }
        | HirExpr::ExternalRef { .. }
        | HirExpr::Identity { .. }
        | HirExpr::Symbol { .. }
        | HirExpr::QueryVar { .. }
        | HirExpr::Hole { .. }
        | HirExpr::Error { .. } => Ok(()),
        HirExpr::Frob { id: _, value, .. } => validate_read_only_expr(semantic, value),
        HirExpr::List { items, .. } => {
            for item in items {
                match item {
                    HirCollectionItem::Expr(expr) | HirCollectionItem::Splice(expr) => {
                        validate_read_only_expr(semantic, expr)?;
                    }
                }
            }
            Ok(())
        }
        HirExpr::Relation { rows, .. } => {
            for expr in rows.iter().flatten() {
                validate_read_only_expr(semantic, expr)?;
            }
            Ok(())
        }
        HirExpr::Map { entries, .. } => {
            for (key, value) in entries {
                validate_read_only_expr(semantic, key)?;
                validate_read_only_expr(semantic, value)?;
            }
            Ok(())
        }
        HirExpr::Unary { expr, .. } => validate_read_only_expr(semantic, expr),
        HirExpr::Binary { left, right, .. } => {
            validate_read_only_expr(semantic, left)?;
            validate_read_only_expr(semantic, right)
        }
        HirExpr::Assign { target, value, .. } => {
            validate_read_only_place(semantic, target)?;
            validate_read_only_expr(semantic, value)
        }
        HirExpr::Call { id, callee, args } => validate_read_only_call(semantic, *id, callee, args),
        HirExpr::RoleDispatch { id, .. } | HirExpr::ReceiverDispatch { id, .. } => Err(
            read_only_query_rejection(semantic, *id, "read-only query cannot invoke methods"),
        ),
        HirExpr::Spawn { id, .. } => Err(read_only_query_rejection(
            semantic,
            *id,
            "read-only query cannot spawn tasks",
        )),
        HirExpr::RelationAtom(atom) => validate_read_only_relation_atom(semantic, atom),
        HirExpr::FactChange { id, .. } => Err(read_only_query_rejection(
            semantic,
            *id,
            "read-only query cannot assert or retract facts",
        )),
        HirExpr::Require { condition, .. } => validate_read_only_expr(semantic, condition),
        HirExpr::Index {
            collection, index, ..
        } => {
            validate_read_only_expr(semantic, collection)?;
            if let Some(index) = index {
                validate_read_only_expr(semantic, index)?;
            }
            Ok(())
        }
        HirExpr::Field { base, .. } => validate_read_only_expr(semantic, base),
        HirExpr::Binding { value, scatter, .. } => {
            if let Some(value) = value {
                validate_read_only_expr(semantic, value)?;
            }
            for binding in scatter {
                if let Some(default) = &binding.default {
                    validate_read_only_expr(semantic, default)?;
                }
            }
            Ok(())
        }
        HirExpr::If {
            condition,
            then_items,
            elseif,
            else_items,
            ..
        } => {
            validate_read_only_expr(semantic, condition)?;
            validate_read_only_items(semantic, then_items)?;
            for (condition, items) in elseif {
                validate_read_only_expr(semantic, condition)?;
                validate_read_only_items(semantic, items)?;
            }
            validate_read_only_items(semantic, else_items)
        }
        HirExpr::Block { items, .. } => validate_read_only_items(semantic, items),
        HirExpr::For { iter, body, .. } => {
            validate_read_only_expr(semantic, iter)?;
            validate_read_only_items(semantic, body)
        }
        HirExpr::While {
            condition, body, ..
        } => {
            validate_read_only_expr(semantic, condition)?;
            validate_read_only_items(semantic, body)
        }
        HirExpr::Return { value, .. } => {
            if let Some(value) = value {
                validate_read_only_expr(semantic, value)?;
            }
            Ok(())
        }
        HirExpr::Raise {
            error,
            message,
            value,
            ..
        } => {
            validate_read_only_expr(semantic, error)?;
            if let Some(message) = message {
                validate_read_only_expr(semantic, message)?;
            }
            if let Some(value) = value {
                validate_read_only_expr(semantic, value)?;
            }
            Ok(())
        }
        HirExpr::Recover { expr, catches, .. } => {
            validate_read_only_expr(semantic, expr)?;
            for catch in catches {
                validate_read_only_recovery(semantic, catch)?;
            }
            Ok(())
        }
        HirExpr::One { expr, .. } => validate_read_only_expr(semantic, expr),
        HirExpr::Break { .. } | HirExpr::Continue { .. } => Ok(()),
        HirExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            validate_read_only_items(semantic, body)?;
            for catch in catches {
                validate_read_only_catch(semantic, catch)?;
            }
            validate_read_only_items(semantic, finally)
        }
        HirExpr::Function { params, body, .. } => {
            for param in params {
                if let Some(default) = &param.default {
                    validate_read_only_expr(semantic, default)?;
                }
            }
            match body {
                HirFunctionBody::Expr(expr) => validate_read_only_expr(semantic, expr),
                HirFunctionBody::Block(items) => validate_read_only_items(semantic, items),
            }
        }
    }
}

fn validate_read_only_call(
    semantic: &mica_compiler::SemanticProgram,
    id: NodeId,
    callee: &HirExpr,
    args: &[HirArg],
) -> Result<(), CompileError> {
    if let HirExpr::ExternalRef { name, .. } = callee
        && !is_safe_read_only_builtin(name)
    {
        return Err(read_only_query_rejection(
            semantic,
            id,
            format!("read-only query cannot call `{name}`"),
        ));
    }
    validate_read_only_expr(semantic, callee)?;
    for arg in args {
        validate_read_only_expr(semantic, &arg.value)?;
    }
    Ok(())
}

fn validate_read_only_relation_atom(
    semantic: &mica_compiler::SemanticProgram,
    atom: &HirRelationAtom,
) -> Result<(), CompileError> {
    for arg in &atom.args {
        validate_read_only_expr(semantic, &arg.value)?;
    }
    Ok(())
}

fn validate_read_only_place(
    semantic: &mica_compiler::SemanticProgram,
    place: &HirPlace,
) -> Result<(), CompileError> {
    match place {
        HirPlace::Local { .. } => Ok(()),
        HirPlace::Index {
            collection, index, ..
        } => {
            validate_read_only_expr(semantic, collection)?;
            if let Some(index) = index {
                validate_read_only_expr(semantic, index)?;
            }
            Ok(())
        }
        HirPlace::Dot { base, .. } => validate_read_only_expr(semantic, base),
        HirPlace::Invalid { id, .. } => Err(read_only_query_rejection(
            semantic,
            *id,
            "read-only query contains an invalid assignment target",
        )),
    }
}

fn validate_read_only_catch(
    semantic: &mica_compiler::SemanticProgram,
    catch: &HirCatch,
) -> Result<(), CompileError> {
    if let Some(condition) = &catch.condition {
        validate_read_only_expr(semantic, condition)?;
    }
    validate_read_only_items(semantic, &catch.body)
}

fn validate_read_only_recovery(
    semantic: &mica_compiler::SemanticProgram,
    recovery: &HirRecovery,
) -> Result<(), CompileError> {
    if let Some(condition) = &recovery.condition {
        validate_read_only_expr(semantic, condition)?;
    }
    validate_read_only_expr(semantic, &recovery.value)
}

fn is_safe_read_only_builtin(name: &str) -> bool {
    matches!(
        name,
        "actor"
            | "principal"
            | "endpoint"
            | "frob"
            | "frob_delegate"
            | "frob_value"
            | "is_frob"
            | "to_literal"
            | "from_literal"
            | "to_symbol"
            | "index_or"
            | "project"
            | "union"
            | "difference"
            | "natural_join"
            | "json_encode"
            | "json_decode"
            | "dom_text"
            | "dom_raw"
            | "dom_element"
            | "dom_html"
            | "to_xml"
            | "from_xml"
            | "dom_diff"
            | "dom_snapshot_payload"
            | "sync_signature"
            | "string_len"
            | "string_chars"
            | "string_slice"
            | "string_from_chars"
            | "string_concat"
            | "string_join"
            | "url_encode_component"
            | "url_decode_component"
            | "words"
            | "string_starts_with"
            | "string_contains"
            | "string_equal_fold"
            | "edit_distance"
            | "parse_ordinal"
            | "lower"
            | "os_getenv"
    )
}

fn read_only_query_rejection(
    semantic: &mica_compiler::SemanticProgram,
    node: NodeId,
    message: impl Into<String>,
) -> CompileError {
    CompileError::Unsupported {
        node,
        span: semantic.span(node).cloned(),
        message: message.into(),
    }
}

fn builtin_symbol_arg(name: &str, args: &[Value], index: usize) -> Result<Symbol, RuntimeError> {
    args.get(index)
        .and_then(Value::as_symbol)
        .ok_or_else(|| invalid_builtin_call(name, "expected symbol argument"))
}

fn builtin_identity_arg(
    name: &str,
    args: &[Value],
    index: usize,
) -> Result<Identity, RuntimeError> {
    args.get(index)
        .and_then(Value::as_identity)
        .ok_or_else(|| invalid_builtin_call(name, "expected identity argument"))
}

pub(crate) fn builtin_string_arg(
    name: &str,
    args: &[Value],
    index: usize,
) -> Result<String, RuntimeError> {
    args.get(index)
        .and_then(|value| value.with_str(str::to_owned))
        .ok_or_else(|| invalid_builtin_call(name, "expected string argument"))
}

pub(crate) fn builtin_char_list_arg(
    name: &str,
    args: &[Value],
    index: usize,
) -> Result<Vec<char>, RuntimeError> {
    args.get(index)
        .and_then(|value| {
            value.with_list(|values| {
                values
                    .iter()
                    .map(|value| {
                        value.with_str(|text| {
                            let mut chars = text.chars();
                            let ch = chars.next()?;
                            chars.next().is_none().then_some(ch)
                        })?
                    })
                    .collect::<Option<Vec<_>>>()
            })
        })
        .flatten()
        .ok_or_else(|| invalid_builtin_call(name, "expected single-character string list argument"))
}

pub(crate) fn builtin_usize_arg(
    name: &str,
    args: &[Value],
    index: usize,
) -> Result<usize, RuntimeError> {
    let Some(value) = args.get(index).and_then(Value::as_int) else {
        return Err(invalid_builtin_call(name, "expected integer argument"));
    };
    usize::try_from(value)
        .map_err(|_| invalid_builtin_call(name, "integer argument is out of range"))
}

fn builtin_arity_arg(name: &str, args: &[Value], index: usize) -> Result<u16, RuntimeError> {
    let Some(arity) = args.get(index).and_then(Value::as_int) else {
        return Err(invalid_builtin_call(name, "expected integer arity"));
    };
    u16::try_from(arity).map_err(|_| invalid_builtin_call(name, "arity must fit in u16"))
}

fn builtin_relation_durability_arg(
    name: &str,
    args: &[Value],
    index: usize,
) -> Result<RelationDurability, RuntimeError> {
    let Some(value) = args.get(index) else {
        return Ok(RelationDurability::Durable);
    };
    match value.as_symbol().and_then(Symbol::name) {
        Some("durable") => Ok(RelationDurability::Durable),
        Some("volatile") => Ok(RelationDurability::Volatile),
        _ => Err(invalid_builtin_call(
            name,
            "relation durability must be :durable or :volatile",
        )),
    }
}

fn builtin_key_positions_arg(
    name: &str,
    args: &[Value],
    index: usize,
    arity: u16,
) -> Result<Vec<u16>, RuntimeError> {
    let Some(value) = args.get(index) else {
        return Err(invalid_builtin_call(name, "expected key position list"));
    };
    let Some(positions) = value.with_list(|values| {
        values
            .iter()
            .map(|value| {
                let Some(position) = value.as_int() else {
                    return Err(invalid_builtin_call(name, "key positions must be integers"));
                };
                let position = u16::try_from(position)
                    .map_err(|_| invalid_builtin_call(name, "key position must fit in u16"))?;
                if position >= arity {
                    return Err(invalid_builtin_call(
                        name,
                        "key position is outside relation arity",
                    ));
                }
                Ok(position)
            })
            .collect::<Result<Vec<_>, _>>()
    }) else {
        return Err(invalid_builtin_call(name, "expected key position list"));
    };
    positions
}

pub(crate) fn invalid_builtin_call(name: &str, message: impl Into<String>) -> RuntimeError {
    RuntimeError::InvalidBuiltinCall {
        name: Symbol::intern(name),
        message: message.into(),
    }
}

pub(crate) fn raised_builtin_error(
    code: &str,
    message: impl Into<String>,
    value: Option<Value>,
) -> RuntimeError {
    RuntimeError::Raised(Value::error(
        Symbol::intern(code),
        Some(message.into()),
        value,
    ))
}

impl RunReport {
    pub fn render(&self) -> String {
        match &self.outcome {
            TaskOutcome::Complete {
                value,
                effects,
                retries,
                ..
            } => render_finished(
                "complete",
                self.task_id,
                value,
                effects,
                *retries,
                &self.identity_names,
                &self.relation_names,
            ),
            TaskOutcome::Aborted {
                error,
                effects,
                retries,
                ..
            } => render_finished(
                "aborted",
                self.task_id,
                error,
                effects,
                *retries,
                &self.identity_names,
                &self.relation_names,
            ),
            TaskOutcome::Suspended {
                kind,
                effects,
                retries,
                ..
            } => {
                let mut out = format!(
                    "task {} suspended: {:?} (retries: {})",
                    self.task_id, kind, retries
                );
                render_effects(
                    &mut out,
                    effects,
                    &self.identity_names,
                    &self.relation_names,
                );
                out
            }
        }
    }
}

pub fn format_source_task_error(error: &SourceTaskError) -> String {
    render_source_task_error(
        error,
        &BTreeMap::new(),
        &BTreeMap::new(),
        None,
        DiagnosticRenderOptions::default(),
    )
}

pub fn format_source_task_error_with_source(
    error: &SourceTaskError,
    source_name: Option<&str>,
    source: &str,
) -> String {
    format_source_task_error_with_source_options(
        error,
        source_name,
        source,
        DiagnosticRenderOptions::source_context(),
    )
}

pub fn format_source_task_error_with_source_options(
    error: &SourceTaskError,
    source_name: Option<&str>,
    source: &str,
    options: DiagnosticRenderOptions,
) -> String {
    render_source_task_error(
        error,
        &BTreeMap::new(),
        &BTreeMap::new(),
        Some(DiagnosticSource::new(source_name, source)),
        options,
    )
}

fn render_source_task_error(
    error: &SourceTaskError,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
    source: Option<DiagnosticSource<'_>>,
    options: DiagnosticRenderOptions,
) -> String {
    match error {
        SourceTaskError::Compile(error) => format_compile_error(error, source, options),
        SourceTaskError::TaskManager(error) => {
            format!(
                "task manager error: {}",
                render_task_manager_error(error, identity_names, relation_names)
            )
        }
    }
}

fn render_task_manager_error(
    error: &TaskManagerError,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    match error {
        TaskManagerError::UnknownTask(task_id) => format!("unknown task {task_id}"),
        TaskManagerError::TaskAlreadyCompleted(task_id) => {
            format!("task {task_id} already completed")
        }
        TaskManagerError::TaskCancelled(task_id) => format!("task {task_id} was cancelled"),
        TaskManagerError::Task(error) => render_task_error(error, identity_names, relation_names),
    }
}

fn render_task_error(
    error: &TaskError,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    match error {
        TaskError::Runtime(error) => render_runtime_error(error, identity_names, relation_names),
        TaskError::ConflictRetriesExceeded { retries } => {
            format!("commit conflict retries exceeded after {retries} retries")
        }
        TaskError::MissingTransaction => "missing transaction".to_owned(),
        TaskError::UnknownRelation(relation) => format!("unknown relation {relation:?}"),
    }
}

fn render_runtime_error(
    error: &RuntimeError,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    match error {
        RuntimeError::InvalidCallable(value) => format!(
            "invalid callable {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::NoApplicableMethod { selector } => format!(
            "no applicable method for {}",
            render_value(selector, identity_names, relation_names)
        ),
        RuntimeError::AmbiguousDispatch { selector, methods } => format!(
            "ambiguous dispatch for {} among {}",
            render_value(selector, identity_names, relation_names),
            render_sequence(
                "[",
                "]",
                methods
                    .iter()
                    .map(|method| render_value(method, identity_names, relation_names))
            )
        ),
        RuntimeError::PermissionDenied { operation, target } => format!(
            "permission denied for {operation} on {}",
            render_value(target, identity_names, relation_names)
        ),
        RuntimeError::MissingMethodProgram { method } => format!(
            "missing method program for {}",
            render_value(method, identity_names, relation_names)
        ),
        RuntimeError::MissingProgramArtifact { program } => format!(
            "missing program artifact for {}",
            render_value(program, identity_names, relation_names)
        ),
        RuntimeError::InvalidRaisedValue(value) => format!(
            "invalid raised value {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidErrorMessage(value) => format!(
            "invalid error message {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidEffectTarget(value) => format!(
            "invalid effect target {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidMailboxCapability {
            operation,
            capability,
        } => format!(
            "invalid mailbox capability for {operation}: {}",
            render_value(capability, identity_names, relation_names)
        ),
        RuntimeError::InvalidSuspendDuration(value) => format!(
            "invalid suspend duration {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidSpawnSelector(value) => format!(
            "invalid spawn selector {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidSpawnRole(value) => format!(
            "invalid spawn role {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidArgumentSplice(value) => format!(
            "invalid argument splice {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::InvalidRelationSplice(value) => format!(
            "invalid relation splice {}",
            render_value(value, identity_names, relation_names)
        ),
        RuntimeError::Raised(value) => {
            format!(
                "raised {}",
                render_value(value, identity_names, relation_names)
            )
        }
        RuntimeError::UnknownBuiltin { name } => {
            format!("unknown builtin {}", render_symbol(*name, ":"))
        }
        RuntimeError::InvalidBuiltinCall { name, message } => {
            format!(
                "invalid builtin call {}: {message}",
                render_symbol(*name, ":")
            )
        }
        RuntimeError::Kernel(error) => render_kernel_error(error, identity_names, relation_names),
        _ => format!("{error:?}"),
    }
}

fn render_kernel_error(
    error: &KernelError,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    let render_relation =
        |relation: &RelationId| render_identity(*relation, identity_names, relation_names);
    match error {
        KernelError::UnknownRelation(relation) => {
            format!("unknown relation {}", render_relation(relation))
        }
        KernelError::UnknownRule(fact_id) => format!("unknown rule fact {fact_id:?}"),
        KernelError::RelationAlreadyExists(relation) => {
            format!("relation {} already exists", render_relation(relation))
        }
        KernelError::ReadOnlyRelation(relation) => {
            format!("relation {} is read-only", render_relation(relation))
        }
        KernelError::MissingRequiredBindings {
            relation,
            positions,
        } => {
            format!(
                "relation {} requires bindings at positions {:?}",
                render_relation(relation),
                positions
            )
        }
        KernelError::ArityMismatch {
            relation,
            expected,
            actual,
        } => format!(
            "relation {} arity mismatch: expected {expected}, actual {actual}",
            render_relation(relation)
        ),
        KernelError::InvalidComputedRelation { relation, message } => {
            format!(
                "invalid computed relation {}: {message}",
                render_relation(relation)
            )
        }
        KernelError::NonPersistentValue { relation, tuple } => {
            format!(
                "relation {} cannot store non-persistent value {}",
                render_relation(relation),
                render_value(
                    &Value::list(tuple.values().to_vec()),
                    identity_names,
                    relation_names
                )
            )
        }
        KernelError::InvalidIndex {
            relation,
            position,
            arity,
        } => format!(
            "invalid index on relation {} at position {position} (arity {arity})",
            render_relation(relation)
        ),
        KernelError::StaleStagedSnapshot { expected, actual } => {
            format!("staged snapshot version {expected} is stale; current version is {actual}")
        }
        KernelError::Persistence(message) => format!("persistence error: {message}"),
        KernelError::DifferentialWeightOverflow {
            relation,
            operation,
            version,
            left,
            right,
        } => format!(
            "differential weight overflow for relation {} during {operation} at version {version}: {left} and {right}",
            render_relation(relation)
        ),
        KernelError::NegativeDifferentialSupport {
            relation,
            tuple,
            version,
            support,
        } => format!(
            "negative differential support {support} for relation {} tuple {} at version {version}",
            render_relation(relation),
            render_value(
                &Value::list(tuple.values().to_vec()),
                identity_names,
                relation_names
            )
        ),
        KernelError::Rule(rule_error) => format!("rule error: {rule_error:?}"),
        KernelError::Conflict(conflict) => format!(
            "commit conflict on relation {} over {}: {:?}",
            render_relation(&conflict.relation),
            render_value(
                &Value::list(conflict.tuple.values().to_vec()),
                identity_names,
                relation_names
            ),
            conflict.kind
        ),
    }
}

fn render_finished(
    label: &str,
    task_id: u64,
    value: &Value,
    effects: &[Emission],
    retries: u8,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    let mut out = format!(
        "task {task_id} {label}: {} (retries: {retries})",
        render_value(value, identity_names, relation_names)
    );
    render_effects(&mut out, effects, identity_names, relation_names);
    out
}

fn render_effects(
    out: &mut String,
    effects: &[Emission],
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) {
    for effect in effects {
        out.push_str("\neffect ");
        out.push_str(&render_value(
            &Value::identity(effect.target()),
            identity_names,
            relation_names,
        ));
        out.push_str(": ");
        out.push_str(&render_value(
            effect.value(),
            identity_names,
            relation_names,
        ));
    }
}

fn render_identity(
    identity: Identity,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    match identity_names.get(&identity) {
        Some(name) => format!("#{name}"),
        None => match relation_names.get(&identity) {
            Some(name) => format!(":{name}"),
            None => {
                if identity == SYSTEM_ENDPOINT {
                    "system-endpoint".to_owned()
                } else {
                    format!("#{}", identity.raw())
                }
            }
        },
    }
}

fn render_value(
    value: &Value,
    identity_names: &BTreeMap<Identity, String>,
    relation_names: &BTreeMap<Identity, String>,
) -> String {
    if value.is_empty_relation() {
        return "nothing".to_owned();
    }
    match value.kind() {
        ValueKind::Bool => value.as_bool().unwrap().to_string(),
        ValueKind::Int => value.as_int().unwrap().to_string(),
        ValueKind::Float => float_to_literal(value.as_float().unwrap()),
        ValueKind::Identity => {
            let identity = value.as_identity().unwrap();
            match identity_names.get(&identity) {
                Some(name) => format!("#{name}"),
                None => match relation_names.get(&identity) {
                    Some(name) => format!("relation(:{name})"),
                    None => format!("#{}", identity.raw()),
                },
            }
        }
        ValueKind::Symbol => render_symbol(value.as_symbol().unwrap(), ":"),
        ValueKind::ErrorCode => render_symbol(value.as_error_code().unwrap(), ""),
        ValueKind::String => value.with_str(|value| format!("{value:?}")).unwrap(),
        ValueKind::Bytes => bytes_literal(value),
        ValueKind::List => value
            .with_list(|values| {
                render_sequence(
                    "[",
                    "]",
                    values
                        .iter()
                        .map(|value| render_value(value, identity_names, relation_names)),
                )
            })
            .unwrap(),
        ValueKind::Map => value
            .with_map(|entries| {
                render_sequence(
                    "[",
                    "]",
                    entries.iter().map(|(key, value)| {
                        format!(
                            "{}: {}",
                            render_value(key, identity_names, relation_names),
                            render_value(value, identity_names, relation_names)
                        )
                    }),
                )
            })
            .unwrap(),
        ValueKind::Range => value
            .with_range(|start, end| match end {
                Some(end) => format!(
                    "{}..{}",
                    render_value(start, identity_names, relation_names),
                    render_value(end, identity_names, relation_names)
                ),
                None => format!("{}.._", render_value(start, identity_names, relation_names)),
            })
            .unwrap(),
        ValueKind::Error => value
            .with_error(|error| {
                let mut out = format!("error({}", render_symbol(error.code(), ""));
                if let Some(message) = error.message() {
                    out.push_str(", ");
                    out.push_str(&format!("{message:?}"));
                }
                if let Some(payload) = error.value() {
                    if error.message().is_none() {
                        out.push_str(", nothing");
                    }
                    out.push_str(", ");
                    out.push_str(&render_value(payload, identity_names, relation_names));
                }
                out.push(')');
                out
            })
            .unwrap(),
        ValueKind::Capability => "<cap>".to_owned(),
        ValueKind::Function => "<function>".to_owned(),
        ValueKind::Relation => value
            .with_relation(|relation| {
                let heading = render_sequence(
                    "[",
                    "]",
                    relation
                        .heading()
                        .iter()
                        .map(|column| render_symbol(*column, ":")),
                );
                let rows = render_sequence(
                    "{",
                    "}",
                    relation.rows().iter().map(|row| {
                        render_sequence(
                            "[",
                            "]",
                            row.values()
                                .iter()
                                .map(|cell| render_value(cell, identity_names, relation_names)),
                        )
                    }),
                );
                format!("{heading} {rows}")
            })
            .unwrap(),
        ValueKind::Frob => value
            .with_frob(|delegate, payload| {
                format!(
                    "{}<{}>",
                    render_value(&Value::identity(delegate), identity_names, relation_names),
                    render_value(payload, identity_names, relation_names)
                )
            })
            .unwrap(),
    }
}

fn render_symbol(symbol: Symbol, prefix: &str) -> String {
    match symbol.name() {
        Some(name) => format!("{prefix}{name}"),
        None => format!("{prefix}#{}", symbol.id()),
    }
}

fn bytes_literal(value: &Value) -> String {
    value
        .with_bytes(|bytes| format!("b\"{}\"", general_purpose::URL_SAFE.encode(bytes)))
        .unwrap()
}

fn render_sequence(open: &str, close: &str, items: impl IntoIterator<Item = String>) -> String {
    let mut out = open.to_owned();
    for (index, item) in items.into_iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        out.push_str(&item);
    }
    out.push_str(close);
    out
}

#[cfg(test)]
mod tests;
