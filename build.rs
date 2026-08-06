/// Compiles the resources listed in `resources/gresource.xml` (currently
/// `style.css`) into a binary blob that is embedded into the executable via
/// `glib::include_resource!` in `src/app.rs`.
fn main() {
    glib_build_tools::compile_resources(
        &["resources"],
        "resources/gresource.xml",
        "audio-library.gresource",
    );
}