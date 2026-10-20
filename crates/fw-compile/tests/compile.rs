//! Compiler behavior: plan shape, fusion, color conversion, keys and errors.

use std::path::PathBuf;
use std::sync::Arc;

use fw_compile::{CompileError, MediaInfo, MediaResolver, Options, Origin, Plan, Step, compile};
use fw_core::id::{IdGen, SequentialIds};
use fw_core::{ColorSpace, ContentHash, NodeId, Time};
use fw_doc::{
    Anchor, Command, Document, Edge, Editor, Interp, Keyframe, Keyframes, Param, PortRef,
};
use fw_plugin::{MediaRef, ParamValue, Registry};

/// Pretends every file is a 64×32 image whose content is its path.
struct FakeMedia {
    space: ColorSpace,
}

impl MediaResolver for FakeMedia {
    fn probe(&self, media: &MediaRef) -> Result<MediaInfo, String> {
        if media.path.starts_with("missing") {
            return Err(format!("{} not found", media.path));
        }
        Ok(MediaInfo {
            path: PathBuf::from(&media.path),
            width: 64,
            height: 32,
            space: self.space,
            fingerprint: ContentHash::of_bytes(media.path.as_bytes()),
            frames: None,
            opaque: true,
            has_audio: false,
            origin: None,
        })
    }
}

const SRGB_FILES: FakeMedia = FakeMedia {
    space: ColorSpace::SRGB,
};

struct Sample {
    editor: Editor,
    read: NodeId,
    exposure: NodeId,
    output: NodeId,
    ids: SequentialIds,
}

fn constant(v: ParamValue) -> Option<Param> {
    Some(Param::Constant(v))
}

fn set(node: NodeId, param: &str, value: Option<Param>) -> Command {
    Command::SetParam {
        node,
        param: param.into(),
        value,
    }
}

/// Read Image → Exposure → Output.
fn sample() -> Sample {
    let mut ids = SequentialIds::new(1);
    let root = ids.next_id();
    let [read, exposure, output] = [(); 3].map(|_| ids.next_id());
    let registry = Arc::new(fw_ops::builtin_registry());
    let mut editor = Editor::new(Document::new(root), registry);
    let add = |id, type_id: &str| Command::AddNode {
        parent: root,
        id,
        type_id: type_id.into(),
        name: None,
        placement: None,
    };
    let connect = |from, to| Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    };
    editor
        .apply(
            "Build",
            vec![
                add(read, "fw.read_image"),
                add(exposure, "fw.exposure"),
                add(output, "fw.output"),
                set(
                    read,
                    "file",
                    constant(ParamValue::Media(MediaRef {
                        path: "cliffs.png".into(),
                    })),
                ),
                set(exposure, "exposure", constant(ParamValue::Float(1.0))),
                connect(read, exposure),
                connect(exposure, output),
            ],
        )
        .unwrap();
    Sample {
        editor,
        read,
        exposure,
        output,
        ids,
    }
}

fn plan_at(s: &Sample, time: &str) -> Plan {
    compile(
        s.editor.doc(),
        s.editor.registry(),
        &SRGB_FILES,
        &Options::at(time.parse().unwrap()),
    )
    .unwrap()
}

fn kernel(plan: &Plan) -> &fw_compile::KernelStep {
    plan.steps
        .iter()
        .find_map(|s| match s {
            Step::Kernel(k) => Some(k),
            _ => None,
        })
        .unwrap()
}

#[test]
fn conversions_are_inserted_and_fused_with_the_node() {
    let s = sample();
    let plan = plan_at(&s, "0");
    println!("{}", plan.describe());
    assert_eq!(plan.steps.len(), 2, "one load and one fused kernel");
    let k = kernel(&plan);
    let labels: Vec<&str> = k.ops.iter().map(|op| op.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "convert sRGB → ACEScg",
            "exposure",
            "convert ACEScg → Linear Rec.709"
        ]
    );
    assert!(matches!(k.ops[0].origin, Origin::Conversion { .. }));
    assert!(matches!(k.ops[1].origin, Origin::Node { node, .. } if node == s.exposure));
    assert_eq!(plan.output_space, ColorSpace::LINEAR_REC709);
    assert_eq!((plan.width, plan.height), (64, 32));
    assert_eq!(plan.output_key(), k.key);
    assert!(plan.describe().contains("fused"));
}

#[test]
fn generated_shaders_are_valid_wgsl() {
    let s = sample();
    let mut shaders = vec![kernel(&plan_at(&s, "0")).wgsl.clone()];
    // Every pair of the offered spaces, as conversions are generated per pair.
    let spaces = [
        "srgb",
        "linear_rec709",
        "acescg",
        "acescct",
        "rec709_display",
    ];
    for from in spaces {
        for to in spaces {
            let mut s = sample();
            let text = |v: &str| constant(ParamValue::Text(v.into()));
            s.editor
                .apply(
                    "Spaces",
                    vec![
                        set(s.read, "space", text(from)),
                        set(s.output, "space", text(to)),
                    ],
                )
                .unwrap();
            shaders.push(kernel(&plan_at(&s, "0")).wgsl.clone());
        }
    }
    for wgsl in shaders {
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}\n{wgsl}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{e:?}\n{wgsl}"));
    }
}

#[test]
fn keys_follow_exactly_what_changes_the_output() {
    let mut s = sample();
    let before = plan_at(&s, "0");
    let load_key = before.steps[0].key();

    // Time doesn't matter when nothing is animated.
    assert_eq!(plan_at(&s, "7/3").output_key(), before.output_key());

    // A parameter change misses the cache for the kernel but not the decoded file.
    s.editor
        .apply(
            "Exposure",
            vec![set(
                s.exposure,
                "exposure",
                constant(ParamValue::Float(2.0)),
            )],
        )
        .unwrap();
    let changed = plan_at(&s, "0");
    assert_ne!(changed.output_key(), before.output_key());
    assert_eq!(changed.steps[0].key(), load_key);

    // Setting it back hits the original key.
    s.editor.undo().unwrap();
    assert_eq!(plan_at(&s, "0").output_key(), before.output_key());

    // Constant animation keys the same as a constant.
    let key = |t: &str, v: f64| Keyframe {
        t: t.parse().unwrap(),
        v: ParamValue::Float(v),
        interp: Interp::Linear,
        anchor: Anchor::Start,
    };
    let flat = Param::Keyframed(Keyframes {
        keyframes: vec![key("0", 1.0), key("2", 1.0)],
    });
    s.editor
        .apply("Flat", vec![set(s.exposure, "exposure", Some(flat))])
        .unwrap();
    assert_eq!(plan_at(&s, "1").output_key(), before.output_key());

    // Real animation keys by the value at each time.
    let ramp = Param::Keyframed(Keyframes {
        keyframes: vec![key("0", 1.0), key("2", 3.0)],
    });
    s.editor
        .apply("Ramp", vec![set(s.exposure, "exposure", Some(ramp))])
        .unwrap();
    assert_eq!(plan_at(&s, "0").output_key(), before.output_key());
    let mid = plan_at(&s, "1");
    assert_ne!(mid.output_key(), before.output_key());
    assert_eq!(kernel(&mid).ops[1].params, vec![2.0]);
    assert_eq!(plan_at(&s, "1").output_key(), mid.output_key());

    // The same content under a different file name keeps the decoded image's
    // key only if the content matches; here the fake content is the path.
    s.editor
        .apply(
            "Relink",
            vec![set(
                s.read,
                "file",
                constant(ParamValue::Media(MediaRef {
                    path: "other.png".into(),
                })),
            )],
        )
        .unwrap();
    assert_ne!(plan_at(&s, "0").steps[0].key(), load_key);
}

#[test]
fn compiled_ids_and_keys_ignore_unrelated_edits() {
    let mut s = sample();
    let before = plan_at(&s, "0");
    let stray = s.ids.next_id();
    s.editor
        .apply(
            "Add stray",
            vec![Command::AddNode {
                parent: s.editor.doc().root(),
                id: stray,
                type_id: "fw.exposure".into(),
                name: None,
                placement: None,
            }],
        )
        .unwrap();
    let after = plan_at(&s, "0");
    assert_eq!(after, before, "an unconnected node changes nothing");
}

#[test]
fn compiling_is_deterministic() {
    let s = sample();
    assert_eq!(plan_at(&s, "1/2"), plan_at(&s, "1/2"));
}

#[test]
fn spaces_that_already_match_need_no_conversion() {
    let mut s = sample();
    let text = |v: &str| constant(ParamValue::Text(v.into()));
    s.editor
        .apply(
            "ACES in and out",
            vec![
                set(s.read, "space", text("acescg")),
                set(s.output, "space", text("acescg")),
            ],
        )
        .unwrap();
    let plan = plan_at(&s, "0");
    let labels: Vec<&str> = kernel(&plan)
        .ops
        .iter()
        .map(|op| op.label.as_str())
        .collect();
    assert_eq!(labels, ["exposure"]);
    assert_eq!(plan.output_space, ColorSpace::ACESCG);

    // "auto" output takes the delivery space from the options.
    s.editor.undo().unwrap();
    let mut options = Options::at(Time::ZERO);
    options.delivery_space = ColorSpace::SRGB;
    let plan = compile(s.editor.doc(), s.editor.registry(), &SRGB_FILES, &options).unwrap();
    assert_eq!(plan.output_space, ColorSpace::SRGB);
}

#[test]
fn problems_name_the_node() {
    let mut s = sample();
    s.editor
        .apply(
            "Missing file",
            vec![set(
                s.read,
                "file",
                constant(ParamValue::Media(MediaRef {
                    path: "missing.png".into(),
                })),
            )],
        )
        .unwrap();
    let err = compile(
        s.editor.doc(),
        s.editor.registry(),
        &SRGB_FILES,
        &Options::at(Time::ZERO),
    )
    .unwrap_err();
    assert!(
        matches!(&err, CompileError::Node { node, .. } if *node == s.read),
        "{err}"
    );
    assert!(err.to_string().contains("Read Image"), "{err}");

    s.editor.undo().unwrap();
    s.editor
        .apply(
            "Disconnect",
            vec![Command::Disconnect {
                input: PortRef::new(s.exposure, "image"),
            }],
        )
        .unwrap();
    let err = compile(
        s.editor.doc(),
        s.editor.registry(),
        &SRGB_FILES,
        &Options::at(Time::ZERO),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not connected"), "{err}");

    s.editor
        .apply("Remove output", vec![Command::RemoveNode { id: s.output }])
        .unwrap();
    let err = compile(
        s.editor.doc(),
        s.editor.registry(),
        &SRGB_FILES,
        &Options::at(Time::ZERO),
    )
    .unwrap_err();
    assert_eq!(err, CompileError::NoOutput);
}

#[test]
fn unknown_nodes_fail_with_their_name() {
    // Saved with a type this registry lacks.
    let mut writer = fw_ops::builtin_registry();
    let mut blur = writer.desc("fw.exposure").unwrap().clone();
    blur.id = "future.blur".into();
    blur.name = "Blur".into();
    writer.register(fw_plugin::NodeType::new(blur)).unwrap();
    let mut s = sample();
    let mut editor = Editor::new(s.editor.doc().clone(), Arc::new(writer));
    let blur_id = s.ids.next_id();
    let reconnect = |from, to| Command::Connect {
        edge: Edge {
            from: PortRef::new(from, "image"),
            to: PortRef::new(to, "image"),
        },
    };
    editor
        .apply(
            "Insert blur",
            vec![
                Command::AddNode {
                    parent: editor.doc().root(),
                    id: blur_id,
                    type_id: "future.blur".into(),
                    name: None,
                    placement: None,
                },
                reconnect(s.exposure, blur_id),
                reconnect(blur_id, s.output),
            ],
        )
        .unwrap();
    let registry: Registry = fw_ops::builtin_registry();
    let doc = fw_doc::format::from_json(&fw_doc::format::to_json(editor.doc()), &registry).unwrap();
    let err = compile(&doc, &registry, &SRGB_FILES, &Options::at(Time::ZERO)).unwrap_err();
    assert!(err.to_string().contains("future.blur"), "{err}");
}

#[test]
fn an_input_node_needs_a_node_that_supplies_an_image() {
    let mut s = sample();
    let input = s.ids.next_id();
    let root = s.editor.doc().root();
    s.editor
        .apply(
            "Input",
            vec![
                Command::AddNode {
                    parent: root,
                    id: input,
                    type_id: "fw.input".into(),
                    name: None,
                    placement: None,
                },
                Command::Connect {
                    edge: Edge {
                        from: PortRef::new(input, "image"),
                        to: PortRef::new(s.exposure, "image"),
                    },
                },
            ],
        )
        .unwrap();
    let err = compile(
        s.editor.doc(),
        s.editor.registry(),
        &SRGB_FILES,
        &Options::at(Time::ZERO),
    )
    .unwrap_err();
    assert!(
        matches!(&err, CompileError::Node { node, .. } if *node == input),
        "{err}"
    );
}
