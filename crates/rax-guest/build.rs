//! Generates the guest data plane services from proto/ateenv/v1alpha/guest.proto.

use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let proto = root.join("ateenv/v1alpha/guest.proto");
    println!("cargo:rerun-if-changed={}", proto.display());

    let fds = protox::compile([&proto], [&root])?;
    let out = PathBuf::from(env::var("OUT_DIR")?);
    std::fs::write(
        out.join("guest_descriptor.bin"),
        prost::Message::encode_to_vec(&fds),
    )?;
    tonic_prost_build::configure().compile_fds(fds)?;
    Ok(())
}
