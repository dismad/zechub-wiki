use anyhow::{Context, Result};
use clap::Parser;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio::time::interval;
use std::env;
use std::fs;
use chrono;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "http://127.0.0.1:8232")]
    rpc_url: String,
    #[arg(long)]
    cookie_file: Option<String>,
    #[arg(short = 'i', long, default_value_t = 5)]
    interval: u64,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Default, Clone)]
struct MempoolInfo {
    size: usize,
    bytes: u64,
}

#[derive(Deserialize, Debug)]
struct BlockchainInfo {
    blocks: u64,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone, Default)]
struct Vin {
    coinbase: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Vout {
    #[serde(rename = "valueZat", default)]
    value_zat: i64,
}

#[derive(Deserialize, Debug, Clone, Default)]
struct Orchard {
    actions: Option<Vec<Value>>,
    #[serde(rename = "valueBalanceZat", default)]
    value_balance_zat: Option<i64>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug, Clone, Default)]
struct RawTx {
    txid: String,
    vin: Vec<Vin>,
    vout: Vec<Vout>,
    #[serde(rename = "vjoinsplit", default)]
    v_joinsplit: Option<Vec<Value>>,
    #[serde(rename = "vShieldedSpend", default)]
    v_shielded_spend: Option<Vec<Value>>,
    #[serde(rename = "vShieldedOutput", default)]
    v_shielded_output: Option<Vec<Value>>,
    orchard: Option<Orchard>,
    #[serde(rename = "valueBalanceZat", default)]
    value_balance_zat: Option<i64>,
    #[serde(rename = "fee", default)]
    fee: Option<f64>,
    #[serde(rename = "vsize", default)]
    vsize: Option<u64>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct TxClass {
    txid: String,
    fee: f64,
    transparent_value: f64,
    sapling_value: f64,
    orchard_value: f64,
    flow_type: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = reqwest::Client::new();
    let rpc = Arc::new(client);
    let auth = if let Some(ref path) = args.cookie_file {
        read_cookie(path)?
    } else {
        auto_detect_cookie()?
    };
    // Fetch initial block height once at startup
    let initial_blocks = get_block_height(&rpc, &auth, &args.rpc_url).await.unwrap_or(0);
    let mut historical_counts: BTreeMap<String, u32> = BTreeMap::new();
    let mut total_tx_seen: u32 = 0;
    let mut seen_txids: HashSet<String> = HashSet::new();
    println!("\x1b[31mzcash mempool monitor - Refresh every {} seconds\x1b[0m", args.interval);
    println!("Full txids + values are printed below — highlight any text with your mouse to copy.");
    println!("Press Ctrl+C to quit and save mempool_data.md\n");
    let mut ticker = interval(Duration::from_secs(args.interval));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                print!("\x1b[2J\x1b[H");
                // Fetch current block height every refresh
                let current_blocks = get_block_height(&rpc, &auth, &args.rpc_url).await.unwrap_or(initial_blocks);
                let blocks_mined = current_blocks.saturating_sub(initial_blocks);
                match fetch_and_classify_mempool(Arc::clone(&rpc), &args, &auth).await {
                    Ok((info, tx_classes, total_fee)) => {
                        for cls in &tx_classes {
                            if seen_txids.insert(cls.txid.clone()) {
                                *historical_counts.entry(cls.flow_type.clone()).or_insert(0) += 1;
                                total_tx_seen += 1;
                            }
                        }
                        println!("\n\x1b[92m=== zcash mempool monitor @ {} ===\x1b[0m\n\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                        println!("Live status : \x1b[93m{}\x1b[0m txs • \x1b[35m{:.3} MB\x1b[0m • Total Fees \x1b[31m{:.8} ZEC\x1b[0m",
                                 info.size, info.bytes as f64 / 1_000_000.0, total_fee);
                        println!("\n\x1b[33mCurrent Shielded Currents:\x1b[0m");
                        let mut current: BTreeMap<String, u32> = BTreeMap::new();
                        for cls in &tx_classes {
                            *current.entry(cls.flow_type.clone()).or_insert(0) += 1;
                        }
                        for (typ, count) in current {
                            println!(" {:<40} {}", typ, count);
                        }
                        println!("----------------------------------------------------------------------");
                        println!("\n\x1b[33mHistorical %\n\x1b[0m");
                        let total = total_tx_seen as f64;

			let transparent = if total > 0.0 {
                            *historical_counts.get("Transparent").unwrap_or(&0) as f64 / total * 100.0
                        } else {
                            0.0
                        };

                        if total > 0.0 {
                            let transparent = *historical_counts.get("Transparent").unwrap_or(&0) as f64 / total * 100.0;
                            println!(" Transparent      : \x1b[91m{:.1}%\x1b[0m", transparent);
                            println!(" Total Shielded   : \x1b[92m{:.1}%\x1b[0m", 100.0 - transparent);
                        }
                        // ← Blocks moved here into the historical section
                        println!(" Blocks Mined     : \x1b[36m{}\x1b[0m", blocks_mined);
                        println!(" Transactions     : \x1b[37m{}\x1b[0m", total_tx_seen);

                        println!("\n\x1b[33mDetailed Shielded Types:\x1b[0m");
                        for (typ, cnt) in historical_counts.iter().filter(|(t, _)| *t != "Transparent") {
                            let pct = if total > 0.0 { (*cnt as f64 / total) * 100.0 } else { 0.0 };
                            println!(" {:<40} {:.1}%", typ, pct);
                        }
                        println!("----------------------------------------------------------------------");

                        // ← NEW SUMMARY LINES (exactly as you asked)
                        // Total Unshielded = ONLY the 5 Deshielding lines
                        let mut deshielding_count: u32 = 0;
                        let mut shielding_count: u32 = 0;
                        let mut private_count: u32 = 0;
                        for (typ, &cnt) in &historical_counts {
                            if typ.starts_with("Deshielding") {
                                deshielding_count += cnt;
                            } else if typ.starts_with("Shielding") {
                                shielding_count += cnt;
                            } else if typ.starts_with("Private") {
                                private_count += cnt;
                            }
                        }
                        let deshielding_pct = if total > 0.0 { (deshielding_count as f64 / total) * 100.0 } else { 0.0 };
                        let shielding_pct = if total > 0.0 { (shielding_count as f64 / total) * 100.0 } else { 0.0 };
                        let private_pct = if total > 0.0 { (private_count as f64 / total) * 100.0 } else { 0.0 };

                        println!(" Total Deshielded : \x1b[91m{:.1}%\x1b[0m", deshielding_pct);
                        println!(" Total Shielding  : \x1b[92m{:.1}%\x1b[0m", shielding_pct);
                        println!(" Total Private    : \x1b[35m{:.1}%\x1b[0m", private_pct);
                        println!("----------------------------------------------------------------------\n");

                        println!("\n\x1b[33mMempool Transactions:\x1b[0m");
                        println!(" {:>3} | {:<64} | {:>14} | {:>12} | {:>12} | {:>12} | {}",
                                 "#", "TxID", "Fee (ZEC)", "T (ZEC)", "S (ZEC)", "O (ZEC)", "Type");
                        println!("{}", "─".repeat(135));
                        for (i, cls) in tx_classes.iter().enumerate() {
                            println!(" {:>3}. | \x1b[32m{:<64}\x1b[0m | \x1b[31m{:>14.8}\x1b[0m | \x1b[94m{:>12.4}\x1b[0m | \x1b[33m{:>12.4}\x1b[0m | \x1b[92m{:>12.4}\x1b[0m | \x1b[97m{}\x1b[0m",
                                     i+1,
                                     cls.txid,
                                     cls.fee,
                                     cls.transparent_value,
                                     cls.sapling_value,
                                     cls.orchard_value,
                                     cls.flow_type);
                        }
                    }
                    Err(e) => println!("Error: {}", e),
                }
            }
            _ = signal::ctrl_c() => {
                save_mempool_data(&historical_counts, total_tx_seen);
                println!("\n\x1b[90mSaved mempool_data.md with historical data!\x1b[0m");
                break;
            }
        }
    }
    Ok(())
}
fn save_mempool_data(historical: &BTreeMap<String, u32>, total: u32) {
    let mut md = String::new();
    md.push_str("# Zebra Mempool Data Snapshot\n\n");
    md.push_str("## Historical Transaction Types (% since start)\n\n");
    md.push_str("| Type | Percentage |\n|------|------------|\n");
    let tot = total as f64;
    for (typ, cnt) in historical {
        let pct = if tot > 0.0 { (*cnt as f64 / tot) * 100.0 } else { 0.0 };
        md.push_str(&format!("| {} | {:.1}% |\n", typ, pct));
    }
    md.push_str(&format!("\n**Total transactions processed:** {}\n", total));
    md.push_str("*File generated on exit.*");
    let _ = fs::write("mempool_data.md", md);
}
fn read_cookie(path: &str) -> Result<Option<(String, String)>> {
    let content = fs::read_to_string(path).context(format!("Cannot read cookie file: {}", path))?;
    let line = content.trim();
    if let Some((user, pass)) = line.split_once(':') {
        Ok(Some((user.to_string(), pass.to_string())))
    } else {
        anyhow::bail!("Invalid cookie format in {}", path)
    }
}
fn auto_detect_cookie() -> Result<Option<(String, String)>> {
    let home = env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let candidates = [
        format!("{}/.cache/zebra/.cookie", home),
        format!("{}/.zcash/.cookie", home),
        "/home/zebra/.cache/zebra/.cookie".to_string(),
    ];
    for path in candidates {
        if Path::new(&path).exists() {
            if let Ok(Some(auth)) = read_cookie(&path) {
                println!("✓ Cookie found: {}", path);
                return Ok(Some(auth));
            }
        }
    }
    println!("No cookie file found - running without authentication");
    Ok(None)
}
async fn get_block_height(rpc: &Arc<reqwest::Client>, auth: &Option<(String, String)>, rpc_url: &str) -> Result<u64> {
    let payload = json!({ "jsonrpc": "2.0", "method": "getblockchaininfo", "params": [], "id": 99 });
    let mut req = rpc.post(rpc_url).json(&payload);
    if let Some((user, pass)) = auth { req = req.basic_auth(user, Some(pass)); }
    let resp: Value = req.send().await?.json().await?;
    let info: BlockchainInfo = serde_json::from_value(resp["result"].clone())?;
    Ok(info.blocks)
}
async fn fetch_and_classify_mempool(
    rpc: Arc<reqwest::Client>,
    args: &Args,
    auth: &Option<(String, String)>,
) -> Result<(MempoolInfo, Vec<TxClass>, f64)> {
    let payload = json!({ "jsonrpc": "2.0", "method": "getmempoolinfo", "params": [], "id": 1 });
    let mut req = rpc.post(&args.rpc_url).json(&payload);
    if let Some((user, pass)) = auth { req = req.basic_auth(user, Some(pass)); }
    let resp: Value = req.send().await?.json().await?;
    let info: MempoolInfo = serde_json::from_value(resp["result"].clone())?;
    let payload = json!({ "jsonrpc": "2.0", "method": "getrawmempool", "params": [true], "id": 2 });
    let mut req = rpc.post(&args.rpc_url).json(&payload);
    if let Some((user, pass)) = auth { req = req.basic_auth(user, Some(pass)); }
    let resp: Value = req.send().await?.json().await?;
    let mempool_entries: HashMap<String, Value> = serde_json::from_value(resp["result"].clone())?;
    let txids: Vec<String> = mempool_entries.keys().cloned().collect();
    let mut txs: Vec<RawTx> = vec![];
    for chunk in txids.chunks(50) {
        let mut requests = vec![];
        for (i, txid) in chunk.iter().enumerate() {
            requests.push(json!({
                "jsonrpc": "2.0",
                "method": "getrawtransaction",
                "params": [txid, 1],
                "id": i
            }));
        }
        let mut req = rpc.post(&args.rpc_url).json(&requests);
        if let Some((user, pass)) = auth { req = req.basic_auth(user, Some(pass)); }
        let responses: Vec<Value> = req.send().await?.json().await?;
        for r in responses {
            if let Some(result) = r.get("result") {
                if let Ok(tx) = serde_json::from_value::<RawTx>(result.clone()) {
                    txs.push(tx);
                }
            }
        }
    }
    let mut tx_classes = vec![];
    let mut total_fee_zec = 0.0;
    for tx in txs {
        let (pool_type, t_zat, s_zat, o_zat) = detect_pools(&tx);
        let transparent_value = t_zat as f64 / 100_000_000.0;
        let sapling_value = s_zat as f64 / 100_000_000.0;
        let orchard_value = o_zat as f64 / 100_000_000.0;
        let flow_type = if pool_type == "Transparent" {
            "Transparent".to_string()
        } else if !pool_type.contains("Transparent") {
            if pool_type == "Orchard" { "Private          : O → O".to_string() }
            else if pool_type == "Sapling" { "Private          : S → S".to_string() }
            else { "Private          : S+O → S+O".to_string() }
        } else {
            match (s_zat.signum(), o_zat.signum()) {
                (-1, 0) => "Shielding        : T → S".to_string(),
                (0, -1) => "Shielding        : T → O".to_string(),
                (-1, -1) => "Shielding        : T → S+O".to_string(),
                (1, 0) => "Deshielding      : S → T".to_string(),
                (0, 1) => "Deshielding      : O → T".to_string(),
                (1, 1) => "Deshielding      : S+O → T".to_string(),
                (1, -1) => "Deshielding      : S → T + T → O".to_string(),
                (-1, 1) => "Deshielding      : O → T + T → S".to_string(),
                _ => if s_zat == 0 && o_zat == 0 { "Zero-net Shielded (Z→Z)".to_string() } else { "Complex Mixed".to_string() },
            }
        };
        let entry = mempool_entries.get(&tx.txid).unwrap_or(&Value::Null);
        let fee = entry["fee"].as_f64().unwrap_or(0.0);
        total_fee_zec += fee;
        tx_classes.push(TxClass {
            txid: tx.txid.clone(),
            fee,
            transparent_value,
            sapling_value,
            orchard_value,
            flow_type,
        });
    }
    Ok((info, tx_classes, total_fee_zec))
}
fn detect_pools(tx: &RawTx) -> (String, i64, i64, i64) {
    let mut pools = vec![];
    if !tx.vin.is_empty() || !tx.vout.is_empty() { pools.push("Transparent".to_string()); }
    if tx.v_joinsplit.as_ref().map_or(false, |v| !v.is_empty()) { pools.push("Sprout".to_string()); }
    if tx.v_shielded_spend.as_ref().map_or(false, |v| !v.is_empty()) || tx.v_shielded_output.as_ref().map_or(false, |v| !v.is_empty()) {
        pools.push("Sapling".to_string());
    }
    if tx.orchard.as_ref().and_then(|o| o.actions.as_ref()).map_or(false, |a| !a.is_empty()) {
        pools.push("Orchard".to_string());
    }
    let pool_type = if pools.is_empty() { "Unknown".to_string() } else { pools.join(",") };
    let t_zat: i64 = tx.vout.iter().map(|v| v.value_zat).sum();
    let s_zat = tx.value_balance_zat.unwrap_or(0);
    let o_zat = tx.orchard.as_ref().and_then(|or| or.value_balance_zat).unwrap_or(0);
    (pool_type, t_zat, s_zat, o_zat)
}
