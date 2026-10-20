use super::*;
use std::io::Write as _;

/// Writes a stored archive with the given entries.
fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
    let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
    for (name, bytes) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

/// A scratch area holding the dropped files (`downloads`) and the shader-pack folder.
struct Scratch {
    _root: tempfile::TempDir,
    downloads: PathBuf,
    packs: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        fs::create_dir(&downloads).unwrap();
        Self {
            packs: root.path().join("shaderpacks"),
            downloads,
            _root: root,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.downloads.join(name)
    }

    fn listed(&self) -> Vec<String> {
        crate::discover_packs(&self.packs)
            .into_iter()
            .map(|pack| pack.name)
            .collect()
    }
}

#[test]
fn dropped_zip_and_folder_packs_are_copied_in_and_listed_while_other_files_are_skipped() {
    let scratch = Scratch::new();
    let zip = scratch.file("Dusk.zip");
    write_zip(&zip, &[("Dusk/shaders/final.fsh", b"void main() {}")]);
    let folder = scratch.file("Cobalt v2");
    fs::create_dir_all(folder.join("Cobalt/shaders/world-1")).unwrap();
    fs::write(folder.join("Cobalt/shaders/final.fsh"), b"// final").unwrap();
    let notes = scratch.file("notes.txt");
    fs::write(&notes, b"").unwrap();

    let outcome = add_dropped_packs(&[notes, zip.clone(), folder], &scratch.packs);

    assert_eq!(
        outcome,
        DropOutcome::Added(vec!["Dusk.zip".into(), "Cobalt v2".into()])
    );
    assert_eq!(scratch.listed(), ["Cobalt v2", "Dusk.zip"]);
    assert_eq!(
        fs::read(scratch.packs.join("Dusk.zip")).unwrap(),
        fs::read(&zip).unwrap()
    );
    assert_eq!(
        fs::read_to_string(scratch.packs.join("Cobalt v2/Cobalt/shaders/final.fsh")).unwrap(),
        "// final"
    );
    assert!(
        scratch
            .packs
            .join("Cobalt v2/Cobalt/shaders/world-1")
            .is_dir()
    );
    assert!(crate::ShaderPack::open(&scratch.packs.join("Dusk.zip")).is_ok());
}

#[test]
fn drops_without_a_pack_are_refused_and_name_a_single_file() {
    let scratch = Scratch::new();
    let not_a_pack = scratch.file("textures.zip");
    write_zip(&not_a_pack, &[("textures/stone.png", b"png")]);
    let corrupt = scratch.file("broken.zip");
    fs::write(&corrupt, b"not a zip").unwrap();
    let deep = scratch.file("Deep");
    fs::create_dir_all(deep.join("a/b/shaders")).unwrap();

    assert_eq!(
        add_dropped_packs(std::slice::from_ref(&not_a_pack), &scratch.packs),
        DropOutcome::NoPacks(Some("textures.zip".into()))
    );
    assert_eq!(
        add_dropped_packs(&[not_a_pack, corrupt, deep], &scratch.packs),
        DropOutcome::NoPacks(None)
    );
    assert!(!scratch.packs.exists(), "refused drops create nothing");
}

#[test]
fn a_pack_already_in_the_folder_is_left_untouched_and_stops_later_packs() {
    let scratch = Scratch::new();
    let first = scratch.file("Amber.zip");
    write_zip(&first, &[("shaders/final.fsh", b"new")]);
    let existing = scratch.file("Bramble.zip");
    write_zip(&existing, &[("shaders/final.fsh", b"new")]);
    let last = scratch.file("Cobalt.zip");
    write_zip(&last, &[("shaders/final.fsh", b"new")]);
    fs::create_dir_all(&scratch.packs).unwrap();
    fs::write(scratch.packs.join("Bramble.zip"), b"installed").unwrap();

    let outcome = add_dropped_packs(&[first, existing, last], &scratch.packs);

    assert_eq!(outcome, DropOutcome::AlreadyExists("Bramble.zip".into()));
    assert_eq!(
        fs::read(scratch.packs.join("Bramble.zip")).unwrap(),
        b"installed"
    );
    assert_eq!(scratch.listed(), ["Amber.zip", "Bramble.zip"]);
}

#[test]
fn a_failed_copy_names_the_pack_and_leaves_no_partial_entry() {
    let scratch = Scratch::new();
    let pack = scratch.file("Dusk.zip");
    write_zip(&pack, &[("shaders/final.fsh", b"")]);
    // A file where the folder belongs makes creating the folder fail.
    fs::write(&scratch.packs, b"").unwrap();

    let outcome = add_dropped_packs(&[pack], &scratch.packs);

    assert!(
        matches!(&outcome, DropOutcome::CopyFailed { name, .. } if name == "Dusk.zip"),
        "{outcome:?}"
    );
    assert!(scratch.packs.is_file());
}

#[test]
fn listing_skips_a_copy_still_being_written() {
    let scratch = Scratch::new();
    fs::create_dir_all(
        scratch
            .packs
            .join(format!("Cobalt{STAGING_SUFFIX}"))
            .join("shaders"),
    )
    .unwrap();
    fs::create_dir_all(scratch.packs.join("Dusk/shaders")).unwrap();
    assert_eq!(scratch.listed(), ["Dusk"]);
}
