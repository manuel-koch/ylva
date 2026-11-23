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

/// Encode a digest as lowercase hexadecimal, including leading zeroes.
pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut digest = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        digest.push(char::from(DIGITS[usize::from(byte >> 4)]));
        digest.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    digest
}
