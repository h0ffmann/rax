//! Generates the AX v1alpha1 types and gRPC service from proto/ax/v1alpha1/ax.proto.
//!
//! protox compiles the proto in pure Rust, so no protoc is needed. prost and
//! tonic generate the messages and service; pbjson generates serde impls that
//! follow the protobuf JSON mapping (lowerCamelCase names, json_name, omitted
//! defaults), the same wire format protojson gives the Go implementation.

use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let proto = root.join("ax/v1alpha1/ax.proto");
    println!("cargo:rerun-if-changed={}", proto.display());

    let fds = protox::compile([&proto], [&root])?;
    let out = PathBuf::from(env::var("OUT_DIR")?);
    let descriptor_path = out.join("ax_descriptor.bin");
    std::fs::write(&descriptor_path, prost::Message::encode_to_vec(&fds))?;

    let mut config = prost_build::Config::new();
    config
        .file_descriptor_set_path(&descriptor_path)
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types");
    tonic_prost_build::configure().compile_fds_with_config(fds, config)?;

    let descriptors = std::fs::read(&descriptor_path)?;
    pbjson_build::Builder::new()
        .register_descriptors(&descriptors)?
        .build(&[".ax.v1alpha1"])?;
    Ok(())
}
