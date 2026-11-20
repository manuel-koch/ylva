// Copyright © 2026 Manuel Koch
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use super::*;
use std::fs;
use uuid::Uuid;

#[test]
fn export_manifest_omits_stdout_digest_if_the_final_flush_fails() {
    struct UnflushableOutput(Vec<u8>);

    impl Write for UnflushableOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    let fixture = tempfile::tempdir().unwrap();
    let chats = fixture.path().join("chats");
    let identity = Uuid::new_v4();
    let key = "test-project";
    let source = crate::store::session_path(&chats, key, identity);
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    let bytes = b"{\"type\":\"example\"}\n";
    fs::write(source, bytes).unwrap();
    let manifest = fixture.path().join("report.json");
    let options = SessionExport {
        session_ids: vec![identity],
        no_subagents: true,
        format: ExportFormat::Jsonl,
        output: None,
        manifest: Some(manifest.clone()),
        raw: true,
        force: false,
    };
    let mut output = UnflushableOutput(Vec::new());
    let mut diagnostics = Vec::new();

    let exit = run_export(
        &options,
        &chats,
        key,
        fixture.path(),
        &mut output,
        &mut diagnostics,
        &|| false,
    )
    .unwrap();

    assert_eq!(exit, u8::from(ExitCode::GeneralError));
    assert_eq!(output.0, bytes);
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
    assert_eq!(manifest["outputs"][0]["publication_status"], "failed");
    assert!(manifest["outputs"][0]["output_sha256"].is_null());
    assert!(String::from_utf8_lossy(&diagnostics).contains("flush failed"));
}

#[test]
fn export_diagnoses_uncertain_manifest_publication_without_changing_its_contents() {
    let fixture = tempfile::tempdir().unwrap();
    let chats = destination::Directory::resolve(&fixture.path().join("chats")).unwrap();
    let path = fixture.path().join("report.json");
    let plan = destination::Plan::preflight(
        &layout::Layout::Stdout,
        [],
        "jsonl",
        &chats,
        Some(&path),
        false,
        &|| false,
    )
    .unwrap();
    let mut publisher = Publisher::new(&chats);
    publisher.sync_published_directory = |_| Err(io::Error::other("manifest sync failed"));
    let inventory = inventory::Inventory::capture(
        vec![],
        false,
        &Store::export_reader(chats.path(), "test-project"),
        fixture.path(),
        &|| false,
    );
    let outcomes = report::Outcomes::default();
    let manifest = report::Manifest::new(
        fixture.path(),
        "test-project",
        fixture.path(),
        &inventory,
        &outcomes,
        false,
        report::Status::Success,
    );
    let destination = plan.manifest.unwrap();

    let result = manifest.publish(&mut publisher, &destination, fixture.path(), &|| false);

    assert!(matches!(result, Err(PublishError::Uncertain(_))));
    let bytes = fs::read(&path).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["status"], "success");
    assert!(record.get("manifest").is_none());
    let mut diagnostics = Vec::new();
    report::manifest_outcome(&mut diagnostics, &path, &result).unwrap();
    assert!(String::from_utf8_lossy(&diagnostics).contains("export manifest uncertain"));
}
