use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use alloy::primitives::U256;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub enum Lane {
    Session,
    Paper,
}

#[derive(Debug, Serialize, Deserialize)]
struct File {
    session: String,
    paper: String,
}

pub struct Nonces {
    path: PathBuf,
    session: u64,
    paper: u64,
}

impl Nonces {
    pub fn load(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            return Ok(Self {
                path,
                session: 0,
                paper: 0,
            });
        }
        let raw = fs::read_to_string(&path)?;
        let file: File = serde_json::from_str(&raw).context("nonce file")?;
        Ok(Self {
            path,
            session: u64::from_str(&file.session).unwrap_or(0),
            paper: u64::from_str(&file.paper).unwrap_or(0),
        })
    }

    pub fn take(&mut self, lane: Lane) -> Result<U256> {
        let slot = match lane {
            Lane::Session => &mut self.session,
            Lane::Paper => &mut self.paper,
        };
        if *slot == 0 {
            *slot = random_seed();
        } else {
            *slot = slot.saturating_add(1);
        }
        let value = *slot;
        self.save()?;
        Ok(U256::from(value))
    }

    fn save(&self) -> Result<()> {
        let file = File {
            session: self.session.to_string(),
            paper: self.paper.to_string(),
        };
        let raw = serde_json::to_string(&file)?;
        fs::write(&self.path, raw)?;
        Ok(())
    }
}

fn random_seed() -> u64 {
    let bytes = PrivateKeySigner::random().to_bytes();
    let n = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
    let n = if n == 0 { 1 } else { n };
    u64::from(n) * 256
}
