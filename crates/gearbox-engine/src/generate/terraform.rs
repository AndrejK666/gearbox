//! Optional Helm-provider deployment of the generated chart into an existing cluster.
use gearbox_ir::{FileEntry, FileKind, Ownership};
use minijinja::context;

use super::{GenerateError, GenerateInput, paths, templates};

pub fn files(input: &GenerateInput<'_>) -> Result<Vec<FileEntry>, GenerateError> {
    let Some(settings) = &input.lock.kubernetes else {
        return Ok(Vec::new());
    };
    let product = &input.lock.product.id;
    let namespace = settings.namespace.as_deref().unwrap_or("default");
    // Product IDs and namespaces are validated Kubernetes names. Serialize data
    // separately from template text so quoting is never hand-built.
    let ctx = context! {
        product => product,
        product_json => serde_json::to_string(product).map_err(|source| GenerateError::Json {what:"Terraform release name",source})?,
        namespace_json => serde_json::to_string(namespace).map_err(|source| GenerateError::Json {what:"Terraform namespace",source})?,
    };
    ["main.tf", "variables.tf", "outputs.tf", "README.md"]
        .into_iter()
        .map(|name| {
            let key = format!("terraform/helm/{name}");
            let body =
                templates::render("Terraform Helm", input.templates.get(&key)?, ctx.clone())?;
            Ok(FileEntry::text(
                paths::rel(&["terraform", name])?,
                body,
                FileKind::Text,
                Ownership::Generated,
            ))
        })
        .collect()
}
