#!/usr/bin/env bash
# Copyright © 2026 Manuel Koch
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Verifies scripts/check-git-conventions.sh against accepted and rejected inputs.

set -uo pipefail

CHECK_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-git-conventions.sh"
WORK_DIRECTORY="$(mktemp -d)"
trap 'rm -rf "${WORK_DIRECTORY}"' EXIT
failures=0

expect() {
    local expected_status="$1" description="$2"
    shift 2
    "$@" >/dev/null 2>&1
    local actual_status=$?
    if [[ ("${expected_status}" == "pass" && ${actual_status} -ne 0) \
        || ("${expected_status}" == "fail" && ${actual_status} -eq 0) ]]; then
        echo "FAIL: ${description} (expected ${expected_status})"
        failures=$((failures + 1))
    fi
}

message_file="${WORK_DIRECTORY}/message"
check_message() {
    printf '%b' "$1" >"${message_file}"
    "${CHECK_SCRIPT}" commit-message "${message_file}"
}

expect pass "plain feat" check_message 'feat: add x\n'
expect pass "scope and breaking marker" check_message 'fix(tui)!: change y\n'
expect pass "trailer" check_message 'chore: z\n\nCo-authored-by: Copilot <1+Copilot@users.noreply.github.com>\n'
expect pass "comment lines ignored" check_message '# hint\nfeat: add x\n'
expect pass "merge commit" check_message "Merge pull request #1 from a/b\n"
expect pass "revert commit" check_message 'Revert "feat: x"\n'
expect pass "fixup commit" check_message 'fixup! feat: x\n'
expect fail "free-form message" check_message 'bad message\n'
expect fail "unknown type" check_message 'wip: stuff\n'
expect fail "missing description" check_message 'feat:\n'
expect fail "empty message" check_message '# only comment\n'

expect pass "branch feature" "${CHECK_SCRIPT}" branch-name feature/foo
expect pass "branch main" "${CHECK_SCRIPT}" branch-name main
expect pass "branch dependabot" "${CHECK_SCRIPT}" branch-name dependabot/cargo/serde-1.0
expect fail "branch Foo_Bar" "${CHECK_SCRIPT}" branch-name Foo_Bar
expect fail "branch without prefix" "${CHECK_SCRIPT}" branch-name foo
expect fail "branch uppercase" "${CHECK_SCRIPT}" branch-name feature/Foo

repository="${WORK_DIRECTORY}/repository"
git init --quiet -b main "${repository}"
git -C "${repository}" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -m "feat: initial"
in_repository() { (cd "${repository}" && "$@"); }

expect fail "commit on main" in_repository "${CHECK_SCRIPT}" not-main
git -C "${repository}" switch --quiet -c feature/foo
expect pass "commit on feature branch" in_repository "${CHECK_SCRIPT}" not-main
git -C "${repository}" switch --quiet -c Foo_Bar
expect fail "current branch invalid" in_repository "${CHECK_SCRIPT}" branch-name
git -C "${repository}" switch --quiet --detach
expect pass "detached HEAD skipped" in_repository "${CHECK_SCRIPT}" branch-name

git -C "${repository}" switch --quiet main
git -C "${repository}" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -m "fix: ok"
git -C "${repository}" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -m "oops"
expect fail "range with bad commit" in_repository "${CHECK_SCRIPT}" commit-range HEAD~2..HEAD
expect pass "range with good commit" in_repository "${CHECK_SCRIPT}" commit-range HEAD~2..HEAD~1

if [[ ${failures} -ne 0 ]]; then
    echo "${failures} check(s) failed"
    exit 1
fi
echo "All git convention checks passed"
