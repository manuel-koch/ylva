use tokio::sync::mpsc;
use ylva::UiUpdate;

/// Consumes `ui_rx` until `done` matches an update, returning every consumed
/// update in order. Panics if more than `budget` updates pass without a match
/// so a runaway loop fails the test instead of hanging.
pub async fn collect_until<F>(ui_rx: &mut mpsc::Receiver<UiUpdate>, done: F) -> Vec<UiUpdate>
where
    F: Fn(&UiUpdate) -> bool,
{
    let mut collected = Vec::new();
    loop {
        let Some(update) = ui_rx.recv().await else {
            return collected;
        };
        let is_done = done(&update);
        collected.push(update.clone());
        if is_done {
            return collected;
        }
        if collected.len() > 500 {
            panic!("collect_until budget exceeded");
        }
    }
}
