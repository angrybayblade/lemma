//! `lm convert`: writing a `lemma.yaml` as an `.lm` root module, so moving
//! an existing configuration is mechanical.

use std::path::Path;

use anyhow::Context;

/// Converts `file`; prints the module, or with `write` writes `mod.lm`
/// beside it, refusing to replace one.
pub(crate) fn run(file: &Path, module: Option<&str>, write: bool) -> anyhow::Result<()> {
    let text =
        std::fs::read_to_string(file).with_context(|| format!("cannot read {}", file.display()))?;
    let parsed =
        lemma_graph::config::parse(&text).with_context(|| format!("invalid {}", file.display()))?;
    let directory = std::path::absolute(file)?
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let name = match module {
        Some(name) => name.to_owned(),
        None => module_name(&directory),
    };
    let converted = lemma_graph::lm::convert(&parsed, &name)
        .with_context(|| format!("cannot convert {}", file.display()))?;
    if !write {
        print!("{converted}");
        return Ok(());
    }
    let target = directory.join(lemma_lang::load::MODULE_FILE);
    anyhow::ensure!(
        !target.exists(),
        "{} exists; remove it or print instead",
        target.display()
    );
    std::fs::write(&target, converted)
        .with_context(|| format!("cannot write {}", target.display()))?;
    eprintln!("wrote {}", target.display());
    Ok(())
}

/// A module name from a directory's name: letters, digits and `_`.
fn module_name(directory: &Path) -> String {
    let raw = directory.file_name().map_or_else(
        || "workspace".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    lemma_graph::lm::repository_name(&raw)
}
