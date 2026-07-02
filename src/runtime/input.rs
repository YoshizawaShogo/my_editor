use crossterm::event::EventStream;
use futures_util::StreamExt;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::input::RawInput;

pub(super) fn spawn(tx: mpsc::UnboundedSender<RawInput>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(result) = events.next().await {
            let raw = match result {
                Ok(event) => match RawInput::try_from(event) {
                    Ok(raw) => raw,
                    Err(()) => continue,
                },
                Err(error) => RawInput::ReadError(format!("入力エラー: {error}")),
            };

            if tx.send(raw).is_err() {
                break;
            }
        }
    })
}
