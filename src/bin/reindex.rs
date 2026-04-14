//! `reindex` — rebuild all three index tiers from the compressed
//! raw store (NOT from lore).
//!
//! Invariant: the compressed store is the source of truth. This
//! binary is how we change tokenizer config, bump `SCHEMA_VERSION`,
//! or recover from a corrupt index without refetching 25M messages.
//!
//! Usage (once the core is wired):
//!     reindex --data-dir /var/klmcp/data
//!
//! Implementation lands in a follow-up PR.
fn main() -> anyhow::Result<()> {
    eprintln!("reindex: stub; not yet implemented (see TODO.md phase 1)");
    Ok(())
}
