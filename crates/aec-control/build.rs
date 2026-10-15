#[path = "../../packaging/windows-resource.rs"]
mod resource;
fn main() {
    resource::build("aec-control");
}
