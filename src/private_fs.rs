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

//! Filesystem helpers for files and directories that may hold secrets.
//!
//! On Unix, created files are readable only by the owner (`0600`) and created
//! directories are accessible only by the owner (`0700`). Existing files and
//! directories keep their permissions. Other platforms use the defaults.

use std::fs::{DirBuilder, OpenOptions};
use std::io;
use std::path::Path;

/// Open options whose creation mode is owner-only on Unix.
pub(crate) fn private_open_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Like [`std::fs::create_dir_all`], creating missing directories owner-only on Unix.
pub(crate) fn create_private_dir_all(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}
