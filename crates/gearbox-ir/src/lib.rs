//! The canonical typed model for Gearbox.
//!
//! Pure data plus construction-time validation: no filesystem, no process, no
//! clock, no network. Everything downstream -- the description-language
//! evaluator, the resolver, the generators, the lock serializer, and the RPC
//! layer -- agrees by agreeing on these types.

pub mod catalogue;
pub mod contract;
pub mod database;
pub use database::{PostgresDatabase, SqliteDatabase};
pub mod diagnostics;
pub mod explain;
pub mod fileset;
pub mod ids;
pub mod intent;
pub mod requirement;
pub mod resolved;
pub mod ui;

pub use catalogue::{
    CargoFeature, Catalogue, ConfigFieldDecl, ConfigFieldType, ConfigSchema, DeclaredRole,
    DesignGear, DocUrls, EndpointDecl, ExtensionPointDecl, GearDescriptor, GearDocs, GitOrigin,
    GtsTypeDecl, Implementation, LifecycleDecl, LoadStage, Located, Maturity, PendingGear,
    PluginImpl, ResolvedSource, RuntimeCap, Scenario, SourceKind, Visibility,
};
pub use contract::{
    CargoRef, ContractDescriptor, ContractKind, ContractVersion, Ecosystem, GrpcProjection,
    PackageRef, ProviderDescriptor, RestProjection, RestVisibility, Transport,
    strip_version_suffix, version_marker,
};
pub use diagnostics::{
    CanonicalErrorId, Diagnostic, DiagnosticCode, DiagnosticDomain, Diagnostics, Location,
    Position, Prevents, Range, RelatedLocation, RuntimeErrorRef, Severity, UnknownDiagnosticCode,
    file_uri,
};
pub use explain::{
    ExplanationGraph, ExplanationNode, NodeKind, ProvenanceEdge, ProvenanceKind, binding_key,
};
pub use fileset::{FileAction, FileEntry, FileKind, FilePlan, FileSet, Ownership};
pub use ids::{
    ApplicationId, CapabilityId, ContractId, GearId, IdError, NodeId, ProductId, ProfileId,
    ProviderId, RelPath, RequirementId, SourceId,
};
pub use intent::{
    ApplicationPin, BindingIntent, BindingMode, ClusterScopeIntent, ConfigValue,
    DeploymentProfileDecl, Discovery, GearSelection, PluginSelection, Preference, ProductIntent,
    ProviderBinding, SourceDecl,
};
pub use requirement::{
    Capability, ClusterPrimitive, ClusterProviderDecl, FeatureGate, Requirement, RequirementKind,
    capabilities,
};
pub use resolved::{
    ApplicationKind, ApplicationRole, BindingMechanism, BindingRequest, Choice, ClusterResolution,
    CutBlocker, CutCandidate, CutSavings, DEFAULT_LAYOUT, Entrypoint, ImageRef, InclusionReason,
    KubernetesSettings, LOCK_SCHEMA_VERSION, ResolvedApplication, ResolvedBinding,
    ResolvedBindingMode, ResolvedClusterBinding, ResolvedEndpoint, ResolvedGear, ResolvedProduct,
    ResolvedProductHeader, Selected, SelfHostedSettings, SpawnSpec, WorkerServe, is_valid_layout,
};
