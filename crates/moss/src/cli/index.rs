// SPDX-FileCopyrightText: Copyright © 2020-2023 Serpent OS Developers
//
// SPDX-License-Identifier: MPL-2.0
use std::{
    collections::{btree_map, BTreeMap},
    io,
    path::{Path, PathBuf, StripPrefixError},
    time::Duration,
};

use clap::{arg, value_parser, ArgMatches, Command};
use futures::{future::BoxFuture, stream, FutureExt, StreamExt, TryStreamExt};
use moss::{
    client, environment,
    package::{self, Meta, MissingMetaFieldError},
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::{fs, task};
use tui::{MultiProgress, ProgressBar, ProgressStyle, Stylize};

pub fn command() -> Command {
    Command::new("index")
        .about("Index a collection of packages")
        .arg(arg!(<INDEX_DIR> "directory of index files").value_parser(value_parser!(PathBuf)))
}

pub async fn handle(args: &ArgMatches) -> Result<(), Error> {
    let dir = args
        .get_one::<PathBuf>("INDEX_DIR")
        .unwrap()
        .canonicalize()?;

    let stone_files = enumerate_stone_files(&dir).await?;

    println!("Indexing {} files\n", stone_files.len());

    let multi_progress = MultiProgress::new();

    let total_progress = multi_progress.add(
        ProgressBar::new(stone_files.len() as u64).with_style(
            ProgressStyle::with_template("\n|{bar:20.cyan/blue}| {pos}/{len}")
                .unwrap()
                .progress_chars("■≡=- "),
        ),
    );
    total_progress.tick();

    let list = stream::iter(&stone_files)
        .map(|path| get_meta(path, &dir, &multi_progress, &total_progress))
        .buffer_unordered(environment::MAX_DISK_CONCURRENCY)
        .try_collect::<Vec<_>>()
        .await?;

    let mut map = BTreeMap::new();

    // Add each meta to the map, removing
    // dupes by keeping the latest release
    for meta in list {
        match map.entry(meta.name.clone()) {
            btree_map::Entry::Vacant(entry) => {
                entry.insert(meta);
            }
            btree_map::Entry::Occupied(mut entry) => {
                match (entry.get().source_release, meta.source_release) {
                    // Error if dupe is same version
                    (prev, curr) if prev == curr => {
                        return Err(Error::DuplicateRelease(
                            meta.name.clone(),
                            meta.source_release,
                        ));
                    }
                    // Update if dupe is newer version
                    (prev, curr) if prev < curr => {
                        entry.insert(meta);
                    }
                    // Otherwise prev is more recent, don't replace
                    _ => {}
                }
            }
        }
    }

    write_index(&dir, map, &total_progress).await?;

    multi_progress.clear()?;

    println!(
        "\nIndex file written to {:?}",
        dir.join("stone.index").display()
    );

    Ok(())
}

async fn write_index(
    dir: &Path,
    map: BTreeMap<package::Name, Meta>,
    total_progress: &ProgressBar,
) -> Result<(), Error> {
    use std::fs::File;

    let dir = dir.to_path_buf();

    total_progress.set_message("Writing index file");
    total_progress.set_style(
        ProgressStyle::with_template("\n {spinner} {wide_msg}")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );
    total_progress.enable_steady_tick(Duration::from_millis(150));

    task::spawn_blocking(move || {
        let mut file = File::create(dir.join("stone.index"))?;

        let mut writer = stone::Writer::new(&mut file, stone::header::v1::FileType::Repository)?;

        for (_, meta) in map {
            let payload = meta.to_stone_payload();
            writer.add_payload(payload.as_slice())?;
        }

        writer.finalize()?;

        Ok(())
    })
    .await
    .expect("join handle")
}

async fn get_meta(
    path: &Path,
    dir: &Path,
    multi_progress: &MultiProgress,
    total_progress: &ProgressBar,
) -> Result<Meta, Error> {
    let relative_path = format!("{}", path.strip_prefix(dir)?.display());

    let progress = multi_progress.insert_before(total_progress, ProgressBar::new_spinner());
    progress.enable_steady_tick(Duration::from_millis(150));

    let (size, hash) = stat_file(path, &relative_path, &progress).await?;

    progress.set_message(format!(
        "{} {}",
        "Indexing".yellow(),
        relative_path.clone().bold(),
    ));
    progress.set_style(
        ProgressStyle::with_template(" {spinner} {wide_msg}")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );

    let (_, payloads) = moss::stone::stream_payloads(path).await?;

    let payloads = payloads.try_collect::<Vec<_>>().await?;

    let payload = payloads
        .iter()
        .find_map(|payload| payload.meta())
        .ok_or(Error::MissingMetaPayload)?;

    let mut meta = Meta::from_stone_payload(&payload.body)?;
    meta.hash = Some(hash);
    meta.download_size = Some(size);
    meta.uri = Some(relative_path.clone());

    progress.finish();
    multi_progress.remove(&progress);
    multi_progress.println(format!("{} {}", "Indexed".green(), relative_path.bold()))?;
    total_progress.inc(1);

    Ok(meta)
}

async fn stat_file(
    path: &Path,
    relative_path: &str,
    progress: &ProgressBar,
) -> Result<(u64, String), Error> {
    use std::fs::File;

    let path = path.to_path_buf();
    let relative_path = relative_path.to_string();
    let progress = progress.clone();

    task::spawn_blocking(move || {
        let file = File::open(path)?;
        let size = file.metadata()?.len();

        progress.set_length(size);
        progress.set_message(format!("{} {}", "Hashing".blue(), relative_path.bold()));
        progress.set_style(
            ProgressStyle::with_template(
                " {spinner} |{percent:>3}%| {wide_msg} {binary_bytes_per_sec:>.dim} ",
            )
            .unwrap()
            .tick_chars("--=≡■≡=--"),
        );

        let mut hasher = Sha256::new();
        io::copy(&mut &file, &mut progress.wrap_write(&mut hasher))?;

        let hash = hex::encode(hasher.finalize());

        Ok((size, hash))
    })
    .await
    .expect("join hande")
}

fn enumerate_stone_files(dir: &Path) -> BoxFuture<Result<Vec<PathBuf>, Error>> {
    async move {
        let mut read_dir = fs::read_dir(dir).await?;

        let mut paths = vec![];

        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            let meta = entry.metadata().await?;

            if meta.is_dir() {
                paths.extend(enumerate_stone_files(&path).await?);
            } else if meta.is_file() && path.extension().and_then(|s| s.to_str()) == Some("stone") {
                paths.push(path);
            }
        }

        Ok(paths)
    }
    .boxed()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("io")]
    Io(#[from] io::Error),

    #[error("stone read")]
    StoneRead(#[from] stone::read::Error),

    #[error("stone write")]
    StoneWrite(#[from] stone::write::Error),

    #[error("package {0} has two files with the same release {1}")]
    DuplicateRelease(package::Name, u64),

    #[error("meta payload missing")]
    MissingMetaPayload,

    #[error(transparent)]
    MissingMetaField(#[from] MissingMetaFieldError),

    #[error(transparent)]
    StipPrefix(#[from] StripPrefixError),

    #[error("client")]
    Client(#[from] client::Error),
}
