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

//! Favorite-owner hooks of the reducer state.

use super::AppState;

impl AppState {
    /// Apply a confirmed favorite change to the visible overlay and to an
    /// overlay parked in `parked_slash_overlay` by an approval or clarify
    /// pause, so restoring the snapshot shows current membership.
    pub fn apply_favorite_membership(
        &mut self,
        provider_connection: &str,
        model_id: &str,
        favorite: bool,
    ) {
        self.slash_overlay
            .apply_favorite_membership(provider_connection, model_id, favorite);
        if let Some(overlay) = self.parked_slash_overlay.as_mut() {
            overlay.apply_favorite_membership(provider_connection, model_id, favorite);
        }
    }
}
