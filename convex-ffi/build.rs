fn main() {
    uniffi::generate_scaffolding("./src/lib.udl").expect("Building the UDL file failed");
}
