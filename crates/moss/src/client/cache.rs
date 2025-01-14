// SPDX-FileCopyrightText: Copyright © 2020-2023 Serpent OS Developers
//
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::PathBuf};

use futures::{stream, StreamExt};
use stone::{payload, read::PayloadKind};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
    runtime::Handle,
    task,
};
use url::Url;

use crate::{environment, package, request, Installation};

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub delta: u64,
    pub completed: u64,
    pub total: u64,
}

impl Progress {
    pub fn pct(&self) -> f32 {
        self.completed as f32 / self.total as f32
    }
}

/// Fetch a package with the provided [`package::Meta`] and [`Installation`] and return a [`Download`] on success.
pub async fn fetch(
    meta: &package::Meta,
    installation: &Installation,
    on_progress: impl Fn(Progress),
) -> Result<Download, Error> {
    let url = meta.uri.as_ref().ok_or(Error::MissingUri)?.parse::<Url>()?;
    let hash = meta.hash.as_ref().ok_or(Error::MissingHash)?;

    let download_path = download_path(installation, hash).await?;

    if fs::try_exists(&download_path).await? {
        return Ok(Download {
            id: meta.id().into(),
            path: download_path,
            installation: installation.clone(),
            was_cached: true,
        });
    }

    let mut bytes = request::get(url).await?;
    let mut out = File::create(&download_path).await?;

    let mut total = 0;

    while let Some(chunk) = bytes.next().await {
        let bytes = chunk?;
        let delta = bytes.len() as u64;
        total += delta;
        out.write_all(&bytes).await?;

        (on_progress)(Progress {
            delta,
            completed: total,
            total: meta.download_size.unwrap_or(total),
        });
    }

    out.flush().await?;

    Ok(Download {
        id: meta.id().into(),
        path: download_path,
        installation: installation.clone(),
        was_cached: false,
    })
}

/// A package that has been downloaded to the installation
pub struct Download {
    id: package::Id,
    path: PathBuf,
    installation: Installation,
    pub was_cached: bool,
}

/// Upon fetch completion we have this unpacked asset bound with
/// an open reader
pub struct UnpackedAsset {
    pub payloads: Vec<PayloadKind>,
}

impl Download {
    /// Unpack the downloaded package
    // TODO: Return an "Unpacked" struct which has a "blit" method on it?
    pub async fn unpack(
        self,
        on_progress: impl Fn(Progress) + Send + 'static,
    ) -> Result<UnpackedAsset, Error> {
        use std::fs::{create_dir_all, remove_file, File};
        use std::io::{copy, Read, Seek, SeekFrom, Write};

        struct ProgressWriter<'a, W> {
            writer: W,
            total: u64,
            written: u64,
            on_progress: &'a dyn Fn(Progress),
        }

        impl<'a, W> ProgressWriter<'a, W> {
            pub fn new(writer: W, total: u64, on_progress: &'a impl Fn(Progress)) -> Self {
                Self {
                    writer,
                    total,
                    written: 0,
                    on_progress,
                }
            }
        }

        impl<'a, W: Write> Write for ProgressWriter<'a, W> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let bytes = self.writer.write(buf)?;

                self.written += bytes as u64;

                (self.on_progress)(Progress {
                    delta: bytes as u64,
                    completed: self.written,
                    total: self.total,
                });

                Ok(bytes)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.writer.flush()
            }
        }

        let rt = Handle::current();

        task::spawn_blocking(move || {
            let content_dir = self.installation.cache_path("content");
            let content_path = content_dir.join(self.id.as_ref());

            create_dir_all(&content_dir)?;

            let mut reader = stone::read(File::open(&self.path)?)?;

            let payloads = reader.payloads()?.collect::<Result<Vec<_>, _>>()?;
            let indicies = payloads
                .iter()
                .filter_map(PayloadKind::index)
                .flat_map(|p| &p.body)
                .collect::<Vec<_>>();

            // If download was cached & all assets exist, we can skip unpacking
            if self.was_cached && rt.block_on(check_assets_exist(&indicies, &self.installation)) {
                return Ok(UnpackedAsset { payloads });
            }

            let content = payloads
                .iter()
                .find_map(PayloadKind::content)
                .ok_or(Error::MissingContent)?;

            let content_file = File::options()
                .read(true)
                .write(true)
                .create(true)
                .open(&content_path)?;

            reader.unpack_content(
                content,
                &mut ProgressWriter::new(&content_file, content.header.plain_size, &on_progress),
            )?;

            indicies
                .into_iter()
                .map(|idx| {
                    // Split file reader over index range
                    let mut file = &content_file;
                    file.seek(SeekFrom::Start(idx.start))?;
                    let mut split_file = (&mut file).take(idx.end - idx.start);

                    let path = rt.block_on(asset_path(
                        &self.installation,
                        &format!("{:02x}", idx.digest),
                    ))?;

                    let mut output = File::create(path)?;

                    copy(&mut split_file, &mut output)?;

                    Ok(())
                })
                .collect::<Result<Vec<_>, Error>>()?;

            remove_file(&content_path)?;

            Ok(UnpackedAsset { payloads })
        })
        .await
        .expect("join handle")
    }
}

/// Returns true if all assets already exist in the installation
async fn check_assets_exist(indicies: &[&payload::Index], installation: &Installation) -> bool {
    stream::iter(indicies)
        .map(|index| async move {
            if let Ok(path) = asset_path(installation, &format!("{:02x}", index.digest)).await {
                return fs::try_exists(path).await.unwrap_or_default();
            }

            false
        })
        .buffer_unordered(environment::MAX_DISK_CONCURRENCY)
        .all(|exists| async move { exists })
        .await
}

pub async fn download_path(installation: &Installation, hash: &str) -> Result<PathBuf, Error> {
    if hash.len() < 5 {
        return Err(Error::MalformedHash(hash.to_string()));
    }

    let directory = installation
        .cache_path("downloads")
        .join("v1")
        .join(&hash[..5])
        .join(&hash[hash.len() - 5..]);

    if !directory.exists() {
        fs::create_dir_all(&directory).await?;
    }

    Ok(directory.join(hash))
}

pub async fn asset_path(installation: &Installation, hash: &str) -> Result<PathBuf, Error> {
    let directory = if hash.len() >= 10 {
        installation
            .assets_path("v2")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash[4..6])
    } else {
        installation.assets_path("v2")
    };

    if !directory.exists() {
        fs::create_dir_all(&directory).await?;
    }

    Ok(directory.join(hash))
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Missing download hash")]
    MissingHash,
    #[error("Missing download URI")]
    MissingUri,
    #[error("Missing content payload")]
    MissingContent,
    #[error("Malformed download hash: {0}")]
    MalformedHash(String),
    #[error("stone format")]
    Format(#[from] stone::read::Error),
    #[error("invalid url")]
    InvalidUrl(#[from] url::ParseError),
    #[error("request")]
    Request(#[from] request::Error),
    #[error("io")]
    Io(#[from] io::Error),
}
