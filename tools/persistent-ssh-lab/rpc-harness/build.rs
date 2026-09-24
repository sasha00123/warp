fn main() {
    let directory = "../../../crates/remote_server/proto";
    println!("cargo:rerun-if-changed={directory}");
    prost_build::compile_protos(&[format!("{directory}/remote_server.proto")], &[directory])
        .expect("compile production SSH-extension protocol");
}
