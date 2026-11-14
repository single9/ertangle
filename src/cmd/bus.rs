//! 匯流排訊息子命令（send、inbox、wait、ask、artifact）。

use anyhow::{bail, Context, Result};
use termade_core::{ArtifactKind, MsgKind, Target};
use termade_protocol::{Frame, Request};
use tr::tr;

use super::{join, resolve_self_session, roundtrip};

pub(crate) fn cmd_send(
    to: Option<String>,
    channel: Option<String>,
    broadcast: bool,
    from: Option<String>,
    kind: String,
    reply_to: Option<String>,
    body: Vec<String>,
) -> Result<()> {
    let target = if broadcast {
        Target::Broadcast
    } else if let Some(c) = channel {
        Target::Channel(c)
    } else if let Some(t) = to {
        Target::parse(&t)
    } else {
        bail!("{}", tr!("請指定 --to、--channel 或 --broadcast"));
    };
    let kind = MsgKind::parse(&kind).with_context(|| {
        tr!(
            "未知的訊息種類：{kind}（chat/handoff/question/answer/status/artifact）",
            kind = kind
        )
    })?;
    let from = from.or_else(|| std::env::var("TERMADE_SESSION").ok());
    match roundtrip(&Request::BusSend {
        from,
        to: target,
        kind,
        body: join(body),
        reply_to,
    })? {
        Frame::Ok { note: Some(n) } => println!("{n}"),
        _ => println!("{}", tr!("已送出訊息")),
    }
    Ok(())
}

pub(crate) fn cmd_inbox(session: Option<String>, unread_only: bool, json: bool) -> Result<()> {
    let session = resolve_self_session(session)?;
    let Frame::Messages { messages, .. } = roundtrip(&Request::BusInbox {
        session,
        unread_only,
    })?
    else {
        bail!("{}", tr!("未取得訊息清單"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&messages)?);
        return Ok(());
    }
    if messages.is_empty() {
        println!("{}", tr!("(收件匣為空)"));
        return Ok(());
    }
    for m in messages {
        println!(
            "{}  [{}] {} → {:?}: {}",
            m.id,
            m.kind.id(),
            m.from,
            m.to,
            m.body
        );
    }
    Ok(())
}

pub(crate) fn cmd_wait(
    session: Option<String>,
    from: Option<String>,
    timeout_ms: u64,
    json: bool,
) -> Result<()> {
    let session = resolve_self_session(session)?;
    let Frame::Messages {
        messages,
        timed_out,
    } = roundtrip(&Request::BusWait {
        session,
        from,
        reply_to: None,
        timeout_ms,
    })?
    else {
        bail!("{}", tr!("等待訊息失敗"));
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "messages": messages,
                "timed_out": timed_out,
            }))?
        );
        return Ok(());
    }
    if timed_out || messages.is_empty() {
        println!("{}", tr!("(等待逾時)"));
        return Ok(());
    }
    for m in messages {
        println!("{}  [{}] {}: {}", m.id, m.kind.id(), m.from, m.body);
    }
    Ok(())
}

pub(crate) fn cmd_ask(
    peer: String,
    session: Option<String>,
    timeout_ms: u64,
    json: bool,
    question: Vec<String>,
) -> Result<()> {
    let session = resolve_self_session(session)?;
    let question_str = join(question);
    let send_frame = roundtrip(&Request::BusSend {
        from: Some(session.clone()),
        to: Target::Direct(peer.clone()),
        kind: MsgKind::Question,
        body: question_str,
        reply_to: None,
    })?;
    let qid = match send_frame {
        Frame::Ok { note: Some(n) } => msg_id_in(&n).unwrap_or(n),
        _ => String::new(),
    };
    let Frame::Messages {
        mut messages,
        timed_out,
    } = roundtrip(&Request::BusWait {
        session,
        from: Some(peer),
        reply_to: if qid.is_empty() {
            None
        } else {
            Some(qid.clone())
        },
        timeout_ms,
    })?
    else {
        bail!("{}", tr!("等待回覆失敗"));
    };
    let reply = messages.pop();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "question_id": qid,
                "reply": reply,
                "timed_out": timed_out,
            }))?
        );
        return Ok(());
    }
    match reply {
        Some(m) => println!("{}", m.body),
        None => println!("{}", tr!("(等待回覆逾時)")),
    }
    Ok(())
}

pub(crate) fn cmd_artifact(
    path: String,
    kind: String,
    note: Option<String>,
    session: Option<String>,
) -> Result<()> {
    let session = resolve_self_session(session)?;
    let kind = ArtifactKind::parse(&kind).with_context(|| {
        tr!(
            "未知的產出種類：{kind}（code/pr/doc/test/run）",
            kind = kind
        )
    })?;
    let frame = roundtrip(&Request::ArtifactShare {
        session,
        kind,
        path,
        note: note.unwrap_or_default(),
    })?;
    if let Frame::Ok { note: Some(n) } = frame {
        println!("{n}");
    }
    Ok(())
}

/// 從 daemon 的回覆附註裡找出訊息 id (`msg_…`)。附註是翻譯過的句子，
/// id 的位置跟語言與路由附註有關，所以不能靠「最後一個字」取得。
fn msg_id_in(note: &str) -> Option<String> {
    let start = note.find("msg_")?;
    let id: String = note[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    Some(id)
}

#[cfg(test)]
mod msg_id_tests {
    use super::msg_id_in;

    #[test]
    fn finds_id_regardless_of_language_and_suffix() {
        assert_eq!(
            msg_id_in("已送出訊息 msg_3f2a1b2c3d4e").as_deref(),
            Some("msg_3f2a1b2c3d4e")
        );
        assert_eq!(
            msg_id_in("Sent message msg_3f2a1b2c3d4e (peer ended)").as_deref(),
            Some("msg_3f2a1b2c3d4e")
        );
        assert_eq!(
            msg_id_in("已送出訊息 msg_ab12（對方已結束）").as_deref(),
            Some("msg_ab12")
        );
        assert_eq!(msg_id_in("no id"), None);
    }
}
