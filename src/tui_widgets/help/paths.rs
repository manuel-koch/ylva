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

//! Home-relative path labels for help metadata.

use std::path::Path;

pub(super) fn format_home_relative_path(path: &Path, home_directory: Option<&Path>) -> String {
    let Some(home_directory) = home_directory else {
        return path.display().to_string();
    };

    match path.strip_prefix(home_directory) {
        Ok(relative_path) if relative_path.as_os_str().is_empty() => "~".to_string(),
        Ok(relative_path) => Path::new("~").join(relative_path).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn home_relative_path_uses_tilde_for_home_and_descendants() {
        let home_directory = Path::new("/Users/manuelkoch");

        assert_eq!(
            format_home_relative_path(home_directory, Some(home_directory)),
            "~"
        );
        assert_eq!(
            format_home_relative_path(
                Path::new("/Users/manuelkoch/workspace/artificial-intelligence/ylva"),
                Some(home_directory),
            ),
            "~/workspace/artificial-intelligence/ylva"
        );
    }

    #[test]
    fn home_relative_path_keeps_paths_outside_home_absolute() {
        assert_eq!(
            format_home_relative_path(
                Path::new("/tmp/ylva-home/profiles/default/config.toml"),
                Some(Path::new("/Users/manuelkoch")),
            ),
            "/tmp/ylva-home/profiles/default/config.toml"
        );
    }
}
