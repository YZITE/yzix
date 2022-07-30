use anyhow::Context as _;
use camino::Utf8PathBuf;
use reqwest::Url;
use std::collections::BTreeMap;
use yzix_client::StoreHash;
use crate::Pattern;

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ItemRawData {
    Fetchurl {
        url: String,
        hash: String,
        #[serde(default)]
        with_path: String,
        #[serde(default)]
        executable: bool,
    },

    Derivation {
        args: Vec<String>,
        #[serde(default)]
        envs: BTreeMap<String, String>,
        #[serde(default)]
        files: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug)]
pub enum ItemData {
    Fetchurl {
        url: reqwest::Url,
        hash: StoreHash,
        with_path: Vec<String>,
        executable: bool,
    },

    Derivation {
        args: Vec<Pattern>,
        envs: BTreeMap<String, Pattern>,
        files: BTreeMap<String, Pattern>,
    },
}

pub type Pkgs = BTreeMap<String, ItemData>;

fn enhance_data(inp: ItemRawData) -> anyhow::Result<ItemData> {
    Ok(match raw {
        ItemRawData::Fetchurl {
            url,
            hash,
            with_path,
            executable,
        } => ItemData::Fetchurl {
            url: url.parse()?,
            hash: hash.parse()?,
            with_path: with_part.split('/').map(|i| i.to_string()).collect(),
            executable,
        },
        ItemRawData::Derivation {
            args,
            envs,
            files,
        } => ItemData::Derivation {
            args: args.into_iter().map(|i| i.parse()).collect::<anyhow::Result<_>>()?,
            envs: args.into_iter().map(|(k, i)| i.parse().map(|j| (k, j))).collect::<anyhow::Result<_>>()?,
            files: args.into_iter().map(|(k, i)| i.parse().map(|j| (k, j))).collect::<anyhow::Result<_>>()?,
        },
        ItemRawData::Import { import } => ItemData::Import(Utf8PathBuf::from(import)),
    })
}

pub fn parse_pkgs_file(fdat: &[u8]) -> anyhow::Result<Pkgs> {
    let raw: BTreeMap<String, ItemRawData> = toml::from_slice(fdat)?;

    raw.into_iter()
        .map(|(k, i)| enhance_data(i).with_context(|| format!("unable to parse entry {:?}", k)).map(|j| (k, j)))
        .collect()
}
