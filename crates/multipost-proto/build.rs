fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .compile_protos(
            &[
                "proto/common.proto",
                "proto/accounts.proto",
                "proto/posts.proto",
                "proto/media.proto",
            ],
            &["proto"],
        )?;
    Ok(())
}
