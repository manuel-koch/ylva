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

/// A top-level application that can be run to completion.
///
/// The returned `u8` is an exit code: `0` means clean exit; errors propagate as `Err`.
/// `main.rs` converts the result to `std::process::exit`.
pub trait App {
    fn run(self) -> impl std::future::Future<Output = anyhow::Result<u8>> + Send;
}
