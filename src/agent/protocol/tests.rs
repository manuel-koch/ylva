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

#[test]
fn construct_and_pattern_match_protocol_enums() {
    assert!(matches!(
        AgentCommand::Prompt("hello".into()),
        AgentCommand::Prompt(text) if text == "hello"
    ));
    assert!(matches!(
        AgentCommand::Stop {
            reason: String::new()
        },
        AgentCommand::Stop { .. }
    ));
    let id = Uuid::new_v4();
    assert!(matches!(
        AgentCommand::ApproveTool { id },
        AgentCommand::ApproveTool { id: got } if got == id
    ));
    assert!(matches!(
        AgentCommand::DenyTool { id, reason: "r".into() },
        AgentCommand::DenyTool { id: got, .. } if got == id
    ));
    assert!(matches!(
        AgentCommand::AnswerClarify("answer".into()),
        AgentCommand::AnswerClarify(answer) if answer == "answer"
    ));

    assert!(matches!(
        AgentEvent::StreamDone { partial: "done".into() },
        AgentEvent::StreamDone { partial } if partial == "done"
    ));
    assert!(matches!(
        AgentEvent::StreamCancelled { partial: "cancel".into() },
        AgentEvent::StreamCancelled { partial } if partial == "cancel"
    ));
    assert!(matches!(
        AgentEvent::StreamError("boom".into()),
        AgentEvent::StreamError(message) if message == "boom"
    ));

    assert!(
        matches!(UiUpdate::AppendText("tok".into()), UiUpdate::AppendText(text) if text == "tok")
    );
    assert!(matches!(
        UiUpdate::AppendThinking("hmm".into()),
        UiUpdate::AppendThinking(text) if text == "hmm"
    ));
    assert!(matches!(UiUpdate::StreamStarted, UiUpdate::StreamStarted));
    assert!(matches!(UiUpdate::StreamDone, UiUpdate::StreamDone));
    assert!(matches!(
        UiUpdate::StreamCancelled,
        UiUpdate::StreamCancelled
    ));
    assert!(matches!(
        UiUpdate::ClarifyPending {
            id,
            question: "question".into(),
            context: None,
            choices: None,
            run_id: None,
        },
        UiUpdate::ClarifyPending {
            id: got,
            question: got_question,
            context: _,
            choices: None,
            run_id: _,
        } if got == id && got_question == "question"
    ));
    assert!(matches!(
        UiUpdate::Error {
            message: "oops".into(),
            run_id: None,
        },
        UiUpdate::Error { message, run_id: None } if message == "oops"
    ));

    assert!(matches!(StreamEvent::Text("tok".into()), StreamEvent::Text(text) if text == "tok"));
    assert!(matches!(
        StreamEvent::Thinking("hmm".into()),
        StreamEvent::Thinking(text) if text == "hmm"
    ));
    assert!(matches!(StreamEvent::Done, StreamEvent::Done));
    assert!(matches!(StreamEvent::Cancelled, StreamEvent::Cancelled));
    assert!(matches!(
        StreamEvent::Error(anyhow::anyhow!("network")),
        StreamEvent::Error(error) if format!("{error}") == "network"
    ));
    assert!(matches!(
        StreamEvent::Delta(DeltaChunk {
            tool_call_index: 0,
            tool_call_id: None,
            function_name: Some("read".into()),
            arguments_fragment: Some("{\"path\":\"a.txt\"}".into()),
        }),
        StreamEvent::Delta(_)
    ));

    assert_eq!(AgentMode::Idle, AgentMode::Idle);
    assert_eq!(AgentMode::Streaming, AgentMode::Streaming);
    assert_eq!(AgentMode::ToolPending, AgentMode::ToolPending);
    assert_eq!(AgentMode::ClarifyPending, AgentMode::ClarifyPending);
}
#[tokio::test]
async fn session_switch_reply_clones_compare_by_identity_and_send_only_once() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let reply = super::SessionSwitchReply::from(sender);
    let duplicate = reply.clone();
    let (other_sender, _) = tokio::sync::oneshot::channel();
    assert_eq!(reply, duplicate);
    assert_ne!(reply, super::SessionSwitchReply::from(other_sender));
    reply.send(true);
    duplicate.send(false);
    assert!(receiver.await.expect("barrier reply"));
}
