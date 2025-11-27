use super::{MarkdownRender, SseEvent};

use crate::config::GlobalConfig;

use crate::utils::{dimmed_text, poll_abort_signal, spawn_spinner, AbortSignal};

use anyhow::Result;
use crossterm::{
    cursor, queue, style,
    terminal::{self, disable_raw_mode, enable_raw_mode},
};
use std::{
    io::{self, stdout, Write},
    time::Duration,
};
use textwrap::core::display_width;
use tokio::sync::mpsc::UnboundedReceiver;

pub async fn markdown_stream(
    rx: UnboundedReceiver<SseEvent>,
    config: &GlobalConfig,
    render: &mut MarkdownRender,
    abort_signal: &AbortSignal,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    let columns = terminal::size()?.0;

    let ret = markdown_stream_inner(rx, config, render, abort_signal, &mut stdout, columns).await;

    disable_raw_mode()?;

    if ret.is_err() {
        println!();
    }
    ret
}

pub async fn raw_stream(
    mut rx: UnboundedReceiver<SseEvent>,
    abort_signal: &AbortSignal,
) -> Result<()> {
    let mut spinner = Some(spawn_spinner("Generating"));

    loop {
        if abort_signal.aborted() {
            break;
        }
        if let Some(evt) = rx.recv().await {
            if let Some(spinner) = spinner.take() {
                spinner.stop();
            }

            match evt {
                SseEvent::Text(text) => {
                    print!("{text}");
                    stdout().flush()?;
                }
                SseEvent::Done => {
                    break;
                }
            }
        }
    }
    if let Some(spinner) = spinner.take() {
        spinner.stop();
    }
    Ok(())
}

async fn markdown_stream_inner<W: Write>(
    mut rx: UnboundedReceiver<SseEvent>,
    config: &GlobalConfig,
    render: &mut MarkdownRender,
    abort_signal: &AbortSignal,
    writer: &mut W,
    columns: u16,
) -> Result<()> {
    let mut buffer = String::new();
    let mut buffer_rows = 1;

    let mut in_think_block = false;
    let mut think_spinner: Option<crate::utils::Spinner> = None;

    let mut spinner = Some(spawn_spinner("Generating"));

    'outer: loop {
        if abort_signal.aborted() {
            break;
        }
        for reply_event in gather_events(&mut rx).await {
            if let Some(spinner) = spinner.take() {
                spinner.stop();
            }

            match reply_event {
                SseEvent::Text(mut text) => {
                    // tab width hacking
                    text = text.replace('\t', "    ");

                    let think_tag_mode = config.read().think_tag_mode.clone();

                    if think_tag_mode == crate::config::ThinkTagMode::Replace {
                        if in_think_block {
                            if let Some(end_pos) = text.find("</think>") {
                                text.replace_range(..end_pos + 8, "");
                                in_think_block = false;
                                if let Some(spinner) = think_spinner.take() {
                                    spinner.stop();
                                }
                            } else {
                                continue;
                            }
                        }
                        while let Some(start) = text.find("<think>") {
                            if let Some(end_rel) = text[start..].find("</think>") {
                                let end = start + end_rel + 8;
                                text.replace_range(start..end, "");
                            } else {
                                text.replace_range(start.., "");
                                in_think_block = true;
                                think_spinner = Some(spawn_spinner("Thinking"));
                                break;
                            }
                        }
                    } else if think_tag_mode == crate::config::ThinkTagMode::Hide {
                        if in_think_block {
                            if let Some(end_pos) = text.find("</think>") {
                                text.replace_range(..end_pos + 8, "");
                                in_think_block = false;
                            } else {
                                continue;
                            }
                        }
                        while let Some(start) = text.find("<think>") {
                            if let Some(end_rel) = text[start..].find("</think>") {
                                let end = start + end_rel + 8;
                                text.replace_range(start..end, "");
                            } else {
                                text.replace_range(start.., "");
                                in_think_block = true;
                                break;
                            }
                        }
                    } else if think_tag_mode == crate::config::ThinkTagMode::Show {
                        if in_think_block {
                            if let Some(end_pos) = text.find("</think>") {
                                let content = &text[..end_pos];
                                let output = dimmed_text(content).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                text.replace_range(..end_pos + 8, "");
                                in_think_block = false;
                            } else {
                                let output = dimmed_text(&text).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                writer.flush()?;
                                continue;
                            }
                        }

                        while let Some(start) = text.find("<think>") {
                            if let Some(end_rel) = text[start..].find("</think>") {
                                let pre_content = &text[..start];
                                if !pre_content.is_empty() {
                                    // Flush buffer before printing think block
                                    if !buffer.is_empty() {
                                        let output = render.render_line(&buffer);
                                        queue!(writer, style::Print(&output))?;
                                        buffer.clear();
                                        buffer_rows = 1; // Reset buffer rows
                                    }
                                    queue!(writer, style::Print(pre_content))?;
                                }
                                
                                // queue!(writer, style::Print(format!("\n{}", dimmed_text("Thinking: "))))?;

                                let content_start = start + 7;
                                let content_end = start + end_rel;
                                let content = &text[content_start..content_end];
                                let output = dimmed_text(content).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                
                                text.replace_range(..content_end + 8, "");
                            } else {
                                let pre_content = &text[..start];
                                // Print content before <think>
                                if !buffer.is_empty() {
                                    // Let's print the buffer using the renderer
                                     let output = render.render_line(&buffer);
                                     queue!(writer, style::Print(&output))?;
                                     buffer.clear();
                                     buffer_rows = 1;
                                }
                                
                                if !pre_content.is_empty() {
                                     queue!(writer, style::Print(pre_content))?;
                                }

                                // queue!(writer, style::Print(format!("\n{}", dimmed_text("Thinking: "))))?;
                                
                                let content = &text[start + 7..];
                                let output = dimmed_text(content).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                writer.flush()?;
                                
                                in_think_block = true;
                                text.clear(); // Consumed everything
                                break;
                            }
                        }
                    } else if think_tag_mode == crate::config::ThinkTagMode::Default {
                        if in_think_block {
                            if let Some(end_pos) = text.find("</think>") {
                                let content = &text[..end_pos + "</think>".len()];
                                let output = dimmed_text(content).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                text.replace_range(..end_pos + "</think>".len(), "");
                                in_think_block = false;
                            } else {
                                let output = dimmed_text(&text).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                writer.flush()?;
                                continue;
                            }
                        }

                        while let Some(start) = text.find("<think>") {
                            if let Some(end_rel) = text[start..].find("</think>") {
                                let pre_content = &text[..start];
                                if !pre_content.is_empty() {
                                    // Flush buffer before printing think block
                                    if !buffer.is_empty() {
                                        let output = render.render_line(&buffer);
                                        queue!(writer, style::Print(&output))?;
                                        buffer.clear();
                                        buffer_rows = 1; // Reset buffer rows
                                    }
                                    queue!(writer, style::Print(pre_content))?;
                                }

                                let end = start + end_rel + "</think>".len();
                                let content = &text[start..end];
                                let output = dimmed_text(content).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                
                                text.replace_range(..end, "");
                            } else {
                                let pre_content = &text[..start];
                                // Print content before <think>
                                if !buffer.is_empty() {
                                    // Let's print the buffer using the renderer
                                     let output = render.render_line(&buffer);
                                     queue!(writer, style::Print(&output))?;
                                     buffer.clear();
                                     buffer_rows = 1;
                                }
                                
                                if !pre_content.is_empty() {
                                     queue!(writer, style::Print(pre_content))?;
                                }

                                let content = &text[start..];
                                let output = dimmed_text(content).replace('\n', "\r\n");
                                queue!(writer, style::Print(output))?;
                                writer.flush()?;
                                
                                in_think_block = true;
                                text.clear(); // Consumed everything
                                break;
                            }
                        }
                    }

                    if text.is_empty() {
                        continue;
                    }

                    let mut attempts = 0;
                    let (col, mut row) = loop {
                        match cursor::position() {
                            Ok(pos) => break pos,
                            Err(_) if attempts < 3 => attempts += 1,
                            Err(e) => return Err(e.into()),
                        }
                    };

                    // Fix unexpected duplicate lines on kitty, see https://github.com/sigoden/aichat/issues/105
                    if col == 0 && row > 0 && display_width(&buffer) == columns as usize {
                        row -= 1;
                    }

                    if row + 1 >= buffer_rows {
                        queue!(writer, cursor::MoveTo(0, row + 1 - buffer_rows),)?;
                    } else {
                        let scroll_rows = buffer_rows - row - 1;
                        queue!(
                            writer,
                            terminal::ScrollUp(scroll_rows),
                            cursor::MoveTo(0, 0),
                        )?;
                    }

                    // No guarantee that text returned by render will not be re-layouted, so it is better to clear it.
                    queue!(writer, terminal::Clear(terminal::ClearType::FromCursorDown))?;

                    if text.contains('\n') {
                        let text = format!("{buffer}{text}");
                        let (head, tail) = split_line_tail(&text);
                        let output = render.render(head);
                        print_block(writer, &output, columns)?;
                        buffer = tail.to_string();
                    } else {
                        buffer = format!("{buffer}{text}");
                    }

                    let output = render.render_line(&buffer);
                    if output.contains('\n') {
                        let (head, tail) = split_line_tail(&output);
                        buffer_rows = print_block(writer, head, columns)?;
                        queue!(writer, style::Print(&tail),)?;

                        // No guarantee the buffer width of the buffer will not exceed the number of columns.
                        // So we calculate the number of rows needed, rather than setting it directly to 1.
                        buffer_rows += need_rows(tail, columns);
                    } else {
                        queue!(writer, style::Print(&output))?;
                        buffer_rows = need_rows(&output, columns);
                    }

                    writer.flush()?;
                }
                SseEvent::Done => {
                    break 'outer;
                }
            }
        }

        if poll_abort_signal(abort_signal)? {
            break;
        }
    }

    if let Some(spinner) = spinner.take() {
        spinner.stop();
    }
    if let Some(spinner) = think_spinner.take() {
        spinner.stop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, ThinkTagMode};
    use parking_lot::RwLock;
    use std::sync::Arc;
    use tokio::sync::mpsc::unbounded_channel;

    #[tokio::test]
    async fn test_markdown_stream_thinking() {
        let mut config = Config::default();
        config.think_tag_mode = ThinkTagMode::Show;
        let config = Arc::new(RwLock::new(config));
        let render_options = crate::render::RenderOptions::default();
        let mut render = MarkdownRender::init(render_options).unwrap();
        let abort_signal = crate::utils::create_abort_signal();
        let (tx, rx) = unbounded_channel();

        let mut writer = Vec::new();
        let columns = 80;

        tokio::spawn(async move {
            tx.send(SseEvent::Text("Hello ".to_string())).unwrap();
            tx.send(SseEvent::Text("<think>Thinking process...\n".to_string())).unwrap();
            tx.send(SseEvent::Text(" More thinking...</think>".to_string())).unwrap();
            tx.send(SseEvent::Text(" Done.".to_string())).unwrap();
            tx.send(SseEvent::Done).unwrap();
        });

        markdown_stream_inner(rx, &config, &mut render, &abort_signal, &mut writer, columns)
            .await
            .unwrap();

        let output = String::from_utf8(writer).unwrap();
        
        // Verify output contains dimmed thinking text
        // Note: dimmed_text adds ANSI codes. We can check for the content and structure.
        assert!(output.contains("Hello"));
        assert!(output.contains("Thinking:"));
        assert!(output.contains("Thinking process..."));
        assert!(output.contains("More thinking..."));
        assert!(output.contains("Done."));
        
        // Verify newlines are replaced with \r\n in thinking block
        // We look for the sequence that corresponds to "...\n" being replaced
        // Since dimmed_text wraps the content, we might see ANSI codes around it.
        // But the replacement happens on the result of dimmed_text.
        // So we expect \r\n to be present.
        assert!(output.contains("\r\n"));
    }
}

async fn gather_events(rx: &mut UnboundedReceiver<SseEvent>) -> Vec<SseEvent> {
    let mut texts = vec![];
    let mut done = false;
    tokio::select! {
        _ = async {
            while let Some(reply_event) = rx.recv().await {
                match reply_event {
                    SseEvent::Text(v) => texts.push(v),
                    SseEvent::Done => {
                        done = true;
                        break;
                    }
                }
            }
        } => {}
        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
    };
    let mut events = vec![];
    if !texts.is_empty() {
        events.push(SseEvent::Text(texts.join("")))
    }
    if done {
        events.push(SseEvent::Done)
    }
    events
}

fn print_block<W: Write>(writer: &mut W, text: &str, columns: u16) -> Result<u16> {
    let mut num = 0;
    for line in text.split('\n') {
        queue!(
            writer,
            style::Print(line),
            style::Print("\n"),
            cursor::MoveLeft(columns),
        )?;
        num += 1;
    }
    Ok(num)
}

fn split_line_tail(text: &str) -> (&str, &str) {
    if let Some((head, tail)) = text.rsplit_once('\n') {
        (head, tail)
    } else {
        ("", text)
    }
}

fn need_rows(text: &str, columns: u16) -> u16 {
    let buffer_width = display_width(text).max(1) as u16;
    buffer_width.div_ceil(columns)
}
