use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::error::{Result, ViaductError};
use crate::models::{Feed, Folder};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct OpmlDocument {
    #[serde(rename = "@version")]
    pub version: String,
    pub head: Head,
    pub body: Body,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Head {
    pub title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Body {
    #[serde(rename = "outline", default)]
    pub outlines: Vec<Outline>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Outline {
    #[serde(rename = "@text")]
    pub text: Option<String>,
    #[serde(rename = "@title")]
    pub title: Option<String>,
    #[serde(rename = "@type")]
    pub type_: Option<String>,
    #[serde(rename = "@xmlUrl")]
    pub xml_url: Option<String>,
    #[serde(rename = "@htmlUrl")]
    pub html_url: Option<String>,
    #[serde(rename = "outline", default)]
    pub outlines: Vec<Outline>,
}

pub struct OpmlFile {
    pub folders: Vec<Folder>,
    pub standalone_feeds: Vec<Feed>,
}

pub fn parse_opml(xml: &str) -> Result<OpmlFile> {
    let doc: OpmlDocument = quick_xml::de::from_str(xml)
        .map_err(|e| ViaductError::Parse(crate::error::ParseError::XmlDe(e.to_string())))?;

    let mut folders = Vec::new();
    let mut standalone_feeds = Vec::new();

    for outline in doc.body.outlines {
        if is_feed(&outline) {
            if let Some(feed) = parse_feed(&outline) {
                standalone_feeds.push(feed);
            }
        } else {
            // It's a folder
            let mut feeds = Vec::new();
            flatten_feeds(&outline.outlines, &mut feeds);

            let name = outline
                .title
                .or(outline.text)
                .unwrap_or_else(|| "Unnamed Folder".to_string());
            folders.push(Folder { name, feeds });
        }
    }

    Ok(OpmlFile {
        folders,
        standalone_feeds,
    })
}

fn is_feed(outline: &Outline) -> bool {
    outline.xml_url.is_some() || outline.type_.as_deref() == Some("rss")
}

fn parse_feed(outline: &Outline) -> Option<Feed> {
    let url = outline.xml_url.clone()?;
    Some(Feed {
        id: url.clone(), // ID is the URL initially
        url,
        name: outline.title.clone().or_else(|| outline.text.clone()),
        edited_name: None,
        home_page_url: outline.html_url.clone(),
    })
}

fn flatten_feeds(outlines: &[Outline], feeds: &mut Vec<Feed>) {
    for outline in outlines {
        if is_feed(outline) {
            if let Some(feed) = parse_feed(outline) {
                feeds.push(feed);
            }
        } else {
            flatten_feeds(&outline.outlines, feeds);
        }
    }
}

pub fn serialize_opml(opml_file: &OpmlFile) -> Result<String> {
    let mut outlines = Vec::new();

    for folder in &opml_file.folders {
        let folder_outlines: Vec<Outline> = folder
            .feeds
            .iter()
            .map(|f| Outline {
                text: f.name.clone(),
                title: f.name.clone(),
                type_: Some("rss".to_string()),
                xml_url: Some(f.url.clone()),
                html_url: f.home_page_url.clone(),
                outlines: Vec::new(),
            })
            .collect();

        outlines.push(Outline {
            text: Some(folder.name.clone()),
            title: Some(folder.name.clone()),
            type_: None,
            xml_url: None,
            html_url: None,
            outlines: folder_outlines,
        });
    }

    for feed in &opml_file.standalone_feeds {
        outlines.push(Outline {
            text: feed.name.clone(),
            title: feed.name.clone(),
            type_: Some("rss".to_string()),
            xml_url: Some(feed.url.clone()),
            html_url: feed.home_page_url.clone(),
            outlines: Vec::new(),
        });
    }

    let doc = OpmlDocument {
        version: "1.0".to_string(),
        head: Head {
            title: Some("viaduct".to_string()),
        },
        body: Body { outlines },
    };

    let mut buf = String::new();
    let mut ser = quick_xml::se::Serializer::new(&mut buf);
    ser.indent(' ', 4);
    doc.serialize(ser)
        .map_err(|e| ViaductError::Parse(crate::error::ParseError::XmlSe(e.to_string())))?;

    Ok(buf)
}

pub struct OpmlWriter {
    sender: mpsc::Sender<OpmlWriterMsg>,
}

enum OpmlWriterMsg {
    Save(OpmlFile, oneshot::Sender<Result<()>>),
}

impl OpmlWriter {
    pub fn spawn(path: impl AsRef<Path> + Send + 'static) -> Self {
        let (tx, mut rx) = mpsc::channel::<OpmlWriterMsg>(10);
        let path = path.as_ref().to_path_buf();

        tokio::spawn(async move {
            let mut pending_save: Option<OpmlFile> = None;
            let mut save_txs: Vec<oneshot::Sender<Result<()>>> = Vec::new();

            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(OpmlWriterMsg::Save(file, reply_tx)) => {
                                pending_save = Some(file);
                                save_txs.push(reply_tx);
                            }
                            None => break, // Channel closed
                        }
                    }
                    _ = sleep(Duration::from_millis(500)), if pending_save.is_some() => {
                        if let Some(file) = pending_save.take() {
                            let res = Self::write_to_disk(&path, &file).await;
                            for reply_tx in save_txs.drain(..) {
                                let _ = reply_tx.send(res.as_ref().map(|_| ()).map_err(|e| crate::error::ViaductError::Io(std::io::Error::other(e.to_string())))); // Needs proper clone of result.
                                // Wait, to make it simpler, we just match on the result and send custom mapped errors or we don't return the result since it's a debounced save.
                                // The original oneshot sender might just wait for completion.
                            }
                        }
                    }
                }
            }
        });

        Self { sender: tx }
    }

    pub async fn save(&self, file: OpmlFile) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(OpmlWriterMsg::Save(file, tx))
            .await
            .map_err(|_| ViaductError::Database(crate::error::DatabaseError::WriterGone))?;
        rx.await.unwrap_or_else(|_| {
            Err(ViaductError::Database(
                crate::error::DatabaseError::WriterGone,
            ))
        })
    }

    async fn write_to_disk(path: &Path, file: &OpmlFile) -> std::io::Result<()> {
        let xml = serialize_opml(file).map_err(|e| std::io::Error::other(e.to_string()))?;

        let temp_path = path.with_extension("opml.tmp");
        tokio::fs::write(&temp_path, xml).await?;
        tokio::fs::rename(temp_path, path).await?;

        Ok(())
    }
}
