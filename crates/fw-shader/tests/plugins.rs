//! Shader plugins read from files: what registers, and what's reported.

use std::path::Path;

use fw_plugin::Registry;
use fw_shader::{plugin, register_folders};

const TINT: &str = r#"
id = "test.tint"
version = 1
name = "Tint"
category = "Color"

[[inputs]]
id = "image"
name = "Image"

[[inputs]]
id = "over"
name = "Over"

[[params]]
id = "color"
name = "Color"
type = "color"
default = [1, 0.5, 0.25]

[[params]]
id = "mode"
name = "Mode"
type = "choice"
options = [{ id = "mix", name = "Mix" }, { id = "add", name = "Add" }]

[[params]]
id = "center"
name = "Center"
type = "vec2"
"#;

const TINT_SHADER: &str = "let d = distance(pos, center) / max(frame.z - frame.x, 1.0);
var out = mix(c, over, d);
if (mode == 1) {
    out = vec4<f32>(out.rgb + color.rgb * out.a, out.a);
}
return out;
";

fn write(folder: &Path, manifest: &str, shader: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join("plugin.toml"), manifest).unwrap();
    std::fs::write(folder.join("shader.wgsl"), shader).unwrap();
}

#[test]
fn a_shader_sees_its_inputs_parameters_and_position_by_name() {
    let plugin = plugin(TINT, TINT_SHADER, "shader.wgsl").unwrap();
    assert_eq!(plugin.problem, None);
    let desc = &plugin.node_type.desc;
    assert_eq!(desc.inputs.len(), 2);
    assert_eq!(desc.params.len(), 3);
}

#[test]
fn a_broken_shader_still_registers_and_says_where_it_went_wrong() {
    let broken = "let a = c.rgb;\nreturn a + nonsense;\n";
    let broken = plugin(TINT, broken, "shader.wgsl").unwrap();
    let problem = broken.problem.expect("the shader doesn't validate");
    assert!(problem.starts_with("shader.wgsl:2:"), "{problem}");
    assert!(problem.contains("nonsense"), "{problem}");

    // Type errors are caught too, not just names, and read as WGSL types.
    let wrong_type = plugin(TINT, "let a = c.rgb;\nreturn a * color;\n", "shader.wgsl").unwrap();
    let problem = wrong_type.problem.expect("vec3 times vec4");
    assert_eq!(
        problem,
        "shader.wgsl:2:8: Operation Multiply can't work with vec3<f32> and vec4<f32>"
    );
}

#[test]
fn editing_the_shader_changes_the_implementation_hash() {
    let a = plugin(TINT, TINT_SHADER, "shader.wgsl").unwrap();
    let b = plugin(
        TINT,
        &TINT_SHADER.replace("mix(c", "mix(c * 0.5"),
        "shader.wgsl",
    )
    .unwrap();
    assert_ne!(a.node_type.impl_hash, b.node_type.impl_hash);
}

#[test]
fn folders_register_their_plugins_and_report_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    write(&first.join("tint"), TINT, TINT_SHADER);
    write(
        &first.join("broken"),
        &TINT.replace("test.tint", "test.broken"),
        "return 1;",
    );
    write(&first.join("unreadable"), "id = ", "");
    write(&second.join("tint-again"), TINT, TINT_SHADER);
    let missing = dir.path().join("missing");

    let mut registry = Registry::new();
    let problems = register_folders(&mut registry, &[first.clone(), second, missing]);
    assert!(registry.get("test.tint").is_some());
    assert!(
        registry.get("test.broken").is_some(),
        "broken shaders still register"
    );
    let messages: Vec<String> = problems.iter().map(ToString::to_string).collect();
    assert_eq!(messages.len(), 3, "{messages:#?}");
    assert!(messages[0].contains("broken") && messages[0].contains("shader"));
    assert!(messages[1].contains("unreadable"));
    assert!(messages[2].contains("tint-again") && messages[2].contains("already loaded"));

    // A folder that is itself a plugin counts as one.
    let mut registry = Registry::new();
    assert!(register_folders(&mut registry, &[first.join("tint")]).is_empty());
    assert!(registry.get("test.tint").is_some());
}
