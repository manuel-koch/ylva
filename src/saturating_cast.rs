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

/// Convert a wire-reported length to `usize`, saturating on 32-bit targets.
pub fn saturating_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Convert a row/column count to a terminal coordinate, saturating at
/// `u16::MAX` instead of wrapping.
pub fn saturating_u16(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_beyond_the_target_range_saturate() {
        assert_eq!(saturating_usize(7), 7);
        assert_eq!(
            saturating_usize(u64::MAX),
            usize::try_from(u64::MAX).unwrap_or(usize::MAX)
        );
    }

    #[test]
    fn counts_beyond_the_terminal_coordinate_range_saturate() {
        assert_eq!(saturating_u16(12), 12);
        assert_eq!(saturating_u16(usize::from(u16::MAX) + 1), u16::MAX);
        assert_eq!(saturating_u16(usize::MAX), u16::MAX);
    }
}
