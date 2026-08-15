//! The filesystem dependency-injection seam driven by its in-memory backend.
//!
//! Run with:
//!
//! ```bash
//! cargo run -p barracuda-fs --example di_seams
//! ```
//!
//! Networking lives in `barracuda-model-api`, where reqwless consumes a concrete
//! `embedded-nal-async` TCP/DNS stack through a platform adapter.

use barracuda_fs::{FileSystem, MemFs};

fn main() -> anyhow::Result<()> {
    filesystem_seam()?;
    Ok(())
}

/// `FileSystem`: byte-oriented persistence. The in-memory `MemFs` behaves like the
/// embedded backend for the operations the modules rely on.
fn filesystem_seam() -> anyhow::Result<()> {
    let filesystem = MemFs::new();

    filesystem.create_dir_all("/data/conversations")?;
    filesystem.write_atomic("/data/conversations/42.json", b"{\"version\":1}")?;
    filesystem.append("/data/conversations/42.jsonl", b"{\"t\":\"group\"}\n")?;

    println!("== FileSystem (MemFs) ==");
    println!(
        "exists  -> {}",
        filesystem.exists("/data/conversations/42.json")
    );
    println!(
        "len     -> {} bytes",
        filesystem.len("/data/conversations/42.jsonl")?
    );
    println!(
        "listing -> {:?}",
        filesystem.list_dir("/data/conversations")?
    );

    // A missing path is a typed error, not a panic.
    println!(
        "missing -> {:?}",
        filesystem.read("/data/conversations/none")
    );
    Ok(())
}
