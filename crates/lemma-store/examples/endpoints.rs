//! Prints the endpoints a store's latest snapshot exports, as
//! `name=symbol`, sorted by name: what a description's endpoints resolved
//! to in code. Usage: `endpoints <index.ls>`.

use std::path::Path;

use lemma_store::Store;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        return Err("usage: endpoints <index.ls>".into());
    };
    let store = Store::open(Path::new(&path))?;
    let Some(snapshot) = store.latest_snapshot()? else {
        return Ok(());
    };
    let mut lines: Vec<String> = store
        .exports(snapshot.id)?
        .into_iter()
        .filter(|export| export.kind.as_str() == "endpoint")
        .map(|export| format!("{}={}", export.name, export.symbol.unwrap_or_default()))
        .collect();
    lines.sort();
    for line in lines {
        println!("{line}");
    }
    Ok(())
}
