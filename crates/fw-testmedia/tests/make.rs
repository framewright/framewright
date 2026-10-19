#[test]
fn every_recipe_produces_a_file() {
    for path in [
        fw_testmedia::count_24fps(),
        fw_testmedia::count_vfr(),
        fw_testmedia::h264(),
        fw_testmedia::count_alpha(),
    ] {
        assert!(
            std::fs::metadata(&path).unwrap().len() > 0,
            "{}",
            path.display()
        );
    }
}
