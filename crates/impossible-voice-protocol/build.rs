//! Build-time generation using a pinned vendored `protoc` executable.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost = tonic_prost_build::Config::new();
    prost.protoc_executable(protoc);
    tonic_prost_build::configure()
        .file_descriptor_set_path(
            std::path::PathBuf::from(std::env::var("OUT_DIR")?)
                .join("impossible_voice_v1_descriptor.bin"),
        )
        .compile_with_config(prost, &["proto/voice.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/voice.proto");
    Ok(())
}
