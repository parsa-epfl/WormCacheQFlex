// Replays a gem5 TLB event trace through the WormCacheQFlex gem5 TLB model.
//
// gem5 vs WormCacheQFlex TLB round-trip test. 
// gem5 writes one trace for its ITB and one for its DTB, with one line per lookup and per TLBI
// (`--tlb-event-trace-dir`). This tool loads the gem5 TLB model state of a
// snapshot (gem5-tlbs-0.json.zstd), feeds the ITLB and the DTLB the same lookups
// and TLBIs, and writes:
//   - the model state at the end, in the same format, and
//   - one event trace per TLB, in gem5's format, so each can be diffed with gem5's.
// By comparing the model state at the end, we can verify whether the gem5 and WormCache models are the same.
//
// Page walks: on a model miss, the translation is taken from the trace line
// being replayed (`TraceISA`). Each lookup line carries gem5's translation for
// that access, whether gem5 hit or missed
//
// Usage:
//   gem5_tlb_replay --state <gem5-tlbs-0.json[.zstd]>
//                   --itb-trace <tlb-events-cpuN.itb.trace> --dtb-trace <tlb-events-cpuN.dtb.trace>
//                   --out-state <file.json.zstd>
//                   --out-itb-trace <file> --out-dtb-trace <file> [--core N]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};

use worm_cache::arch::{ISA, MiscRegs, PageSize, TranslationResult};
use worm_cache::components::cache_hierarchy::mmu::MMUFlushMode;
use worm_cache::components::cache_hierarchy::mmu::gem5_tlb::{Gem5L1TLBs, Gem5TLBEntry};
use worm_cache::components::cache_hierarchy::mmu::tlb::AddressSpaceID;
use worm_cache::parameter::{DTLB_ASSO, ITLB_ASSO};

type Tlbs = Gem5L1TLBs<ITLB_ASSO, DTLB_ASSO>;

// The translation of the trace line being replayed.
#[derive(Default, Clone)]
struct Current {
    asid: u16,
    paddr: u64,
    n: u8,
    global: bool,
    misc_regs: MiscRegs,
}

thread_local! {
    static CURRENT: RefCell<Current> = RefCell::new(Current::default());
}

// Answers the model's register reads and page walks from the current trace line.
struct TraceISA;

impl ISA for TraceISA {
    fn get_asid() -> u16 {
        CURRENT.with(|c| c.borrow().asid)
    }

    fn ptw(_va: u64) -> TranslationResult {
        CURRENT.with(|c| {
            let c = c.borrow();
            TranslationResult {
                paddr: c.paddr,
                is_global: c.global,
                page_size: match c.n {
                    12 => PageSize::_4KB,
                    21 => PageSize::_2MB,
                    30 => PageSize::_1GB,
                    n => panic!("unsupported page-size shift {n} in trace"),
                },
                traces: [0; 4],
                cacheable: true,
            }
        })
    }

    fn get_misc_regs() -> MiscRegs {
        CURRENT.with(|c| c.borrow().misc_regs.clone())
    }
}

fn parse_hex(s: &str) -> u64 {
    let digits = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(digits, 16).unwrap_or_else(|_| panic!("bad hex value {s:?}"))
}

// gem5 sign-extends TLBI addresses from bit 55 (_flushMva: sext<56>).
fn sext56(va: u64) -> u64 {
    if va & (1 << 55) != 0 {
        va | 0xff00_0000_0000_0000
    } else {
        va
    }
}

// Same entry format as gem5's TLB::traceEntry: vpn:N:asid, "G" for global.
fn fmt_entry(e: &Gem5TLBEntry) -> String {
    let asid = match e.asid {
        AddressSpaceID::Global => "G".to_string(),
        AddressSpaceID::NonGlobal(a) => a.to_string(),
    };
    format!("0x{:x}:{}:{}", e.vpn, e.n, asid)
}

fn same_mapping(a: &Gem5TLBEntry, b: &Gem5TLBEntry) -> bool {
    a.vpn == b.vpn && a.n == b.n && a.asid == b.asid && a.ppn == b.ppn
}

fn open_maybe_zstd(path: &str) -> Box<dyn Read> {
    let file = File::open(path).unwrap_or_else(|e| panic!("cannot open {path}: {e}"));
    if path.ends_with(".zst") || path.ends_with(".zstd") {
        Box::new(zstd::Decoder::new(file).unwrap())
    } else {
        Box::new(file)
    }
}

struct Args {
    state: String,
    itb_trace: String,
    dtb_trace: String,
    out_state: String,
    out_itb_trace: String,
    out_dtb_trace: String,
    core: usize,
}

fn parse_args() -> Args {
    let mut map = HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(key) = it.next() {
        let value = it.next().unwrap_or_else(|| panic!("missing value for {key}"));
        map.insert(key, value);
    }
    let mut take = |k: &str| {
        map.remove(k).unwrap_or_else(|| {
            eprintln!(
                "usage: gem5_tlb_replay --state <gem5-tlbs-0.json[.zstd]> \
                 --itb-trace <trace> --dtb-trace <trace> --out-state <file.json.zstd> \
                 --out-itb-trace <file> --out-dtb-trace <file> [--core N]"
            );
            std::process::exit(2)
        })
    };
    let args = Args {
        state: take("--state"),
        itb_trace: take("--itb-trace"),
        dtb_trace: take("--dtb-trace"),
        out_state: take("--out-state"),
        out_itb_trace: take("--out-itb-trace"),
        out_dtb_trace: take("--out-dtb-trace"),
        core: map.remove("--core").map(|c| c.parse().unwrap()).unwrap_or(0),
    };
    if let Some(k) = map.keys().next() {
        panic!("unknown argument {k}");
    }
    args
}

#[derive(Default)]
struct Counts {
    lookups: u64,
    hits: u64,
    misses: u64,
    tlbis: u64,
    lines_differing_from_gem5: u64,
}

// The entries of the ITLB or the DTLB.
fn tlb_entries(tlbs: &Tlbs, is_instruction: bool) -> &[Gem5TLBEntry] {
    if is_instruction {
        tlbs.itlb.entries()
    } else {
        tlbs.dtlb.entries()
    }
}

// Flushes only the ITLB or only the DTLB. gem5 applies a TLBI to each of its
// TLBs separately and writes a line in each trace, so the replay does the same.
fn flush_one(tlbs: &mut Tlbs, is_instruction: bool, mode: MMUFlushMode) {
    if is_instruction {
        tlbs.itlb.flush(mode);
    } else {
        tlbs.dtlb.flush(mode);
    }
}

// Replays one TLB's trace through that TLB of the model.
fn replay_trace(tlbs: &mut Tlbs, is_instruction: bool, trace: &str, out_trace: &str) -> Counts {
    // Recency only matters relative to other entries of this TLB, so continue
    // the restored timestamps and add one per lookup.
    let mut ts = tlb_entries(tlbs, is_instruction).iter().map(|e| e.ts).max().unwrap_or(0) + 1;

    let input = BufReader::new(open_maybe_zstd(trace));
    let mut out = BufWriter::new(File::create(out_trace).expect("cannot create out trace"));
    let mut counts = Counts::default();

    for (line_no, line) in input.lines().enumerate() {
        let line = line.unwrap();
        let toks: Vec<&str> = line.split_whitespace().collect();
        if toks.is_empty() {
            continue;
        }
        let (kind, seq) = (toks[0], toks[1]);
        let f: HashMap<&str, &str> = toks[2..]
            .iter()
            .map(|t| t.split_once('=').unwrap_or_else(|| panic!("line {}: bad field {t}", line_no + 1)))
            .collect();

        let replayed = match kind {
            "L" => {
                counts.lookups += 1;
                let va = parse_hex(f["va"]);
                let n: u8 = f["n"].parse().unwrap();
                CURRENT.with(|c| {
                    *c.borrow_mut() = Current {
                        asid: f["asid"].parse().unwrap(),
                        paddr: parse_hex(f["pa"]),
                        n,
                        global: f["g"] == "1",
                        misc_regs: MiscRegs {
                            cpsr: parse_hex(f["cpsr"]),
                            sctlr_el1: parse_hex(f["sctlr"]),
                            tcr_el1: parse_hex(f["tcr"]),
                            ttbr0_el1: parse_hex(f["ttbr0"]),
                            ttbr1_el1: parse_hex(f["ttbr1"]),
                            mair_el1: parse_hex(f["mair"]),
                        },
                    }
                });

                let before = tlb_entries(tlbs, is_instruction).to_vec();
                tlbs.access::<TraceISA>(va, ts, is_instruction);
                let after = tlb_entries(tlbs, is_instruction);

                // A hit changes one entry's ts in place; a miss pushes a new
                // entry or replaces the evicted one at its index.
                let (res, index, entry, evicted) = if after.len() > before.len() {
                    ("M", None, after.last().unwrap().clone(), None)
                } else {
                    let i = (0..before.len())
                        .find(|&i| before[i].ts != after[i].ts || !same_mapping(&before[i], &after[i]))
                        .expect("the model changed no entry");
                    if same_mapping(&before[i], &after[i]) {
                        // Index of the hit entry in recency order: the number of
                        // entries used more recently (0 = most recent). gem5's
                        // index= counts the same way (valid entries only).
                        let index = before.iter().filter(|e| e.ts > before[i].ts).count();
                        ("H", Some(index), after[i].clone(), None)
                    } else {
                        ("M", None, after[i].clone(), Some(before[i].clone()))
                    }
                };
                ts += 1;
                if res == "H" {
                    counts.hits += 1;
                } else {
                    counts.misses += 1;
                }

                let mask = (1u64 << entry.n) - 1;
                let pa = (entry.ppn << entry.n) | (va & mask);
                let global = matches!(entry.asid, AddressSpaceID::Global);
                format!(
                    "L {seq} va={} asid={} res={res} index={} vpn=0x{:x} n={} pfn=0x{:x} pa=0x{:x} g={} \
                     cpsr={} sctlr={} tcr={} ttbr0={} ttbr1={} mair={} ev={}",
                    f["va"],
                    f["asid"],
                    index.map_or("-".to_string(), |r| r.to_string()),
                    entry.vpn,
                    entry.n,
                    entry.ppn,
                    pa,
                    global as u8,
                    f["cpsr"],
                    f["sctlr"],
                    f["tcr"],
                    f["ttbr0"],
                    f["ttbr1"],
                    f["mair"],
                    evicted.as_ref().map_or("-".to_string(), fmt_entry),
                )
            }
            "T" => {
                counts.tlbis += 1;
                let asid = || AddressSpaceID::NonGlobal(f["asid"].parse().unwrap());
                let vpn = || sext56(parse_hex(f["va"])) >> 12;
                let mode = match f["op"] {
                    "ALL" => MMUFlushMode::All,
                    "ASID" => MMUFlushMode::ByASID(asid()),
                    "VA" => MMUFlushMode::ByVPN(vpn(), 1),
                    "VAASID" => MMUFlushMode::ByVPNAndASID(vpn(), 1, asid()),
                    op => panic!("line {}: unknown TLBI op {op}", line_no + 1),
                };

                let before = tlb_entries(tlbs, is_instruction).to_vec();
                flush_one(tlbs, is_instruction, mode);
                let after = tlb_entries(tlbs, is_instruction);
                // gem5 lists removed entries in table order, most recent first.
                let mut removed: Vec<&Gem5TLBEntry> = before
                    .iter()
                    .filter(|b| !after.iter().any(|a| a.ts == b.ts && same_mapping(a, b)))
                    .collect();
                removed.sort_by(|a, b| b.ts.cmp(&a.ts));
                let rm = if removed.is_empty() {
                    "-".to_string()
                } else {
                    removed.iter().map(|e| fmt_entry(e)).collect::<Vec<_>>().join(",")
                };
                format!("T {seq} op={} asid={} va={} rm={rm}", f["op"], f["asid"], f["va"])
            }
            other => panic!("line {}: unknown event kind {other}", line_no + 1),
        };

        if replayed != line {
            counts.lines_differing_from_gem5 += 1;
        }
        writeln!(out, "{replayed}").unwrap();
    }
    out.flush().unwrap();
    counts
}

fn print_counts(name: &str, c: &Counts) {
    eprintln!(
        "{name}: lookups={} hits={} misses={} tlbis={} lines_differing_from_gem5={}",
        c.lookups, c.hits, c.misses, c.tlbis, c.lines_differing_from_gem5
    );
}

fn main() {
    let args = parse_args();

    let mut cores: Vec<Tlbs> = serde_json::from_reader(open_maybe_zstd(&args.state))
        .expect("cannot parse the gem5 TLB model state");
    assert!(args.core < cores.len(), "core {} not in state file", args.core);
    let tlbs = &mut cores[args.core];

    let itb = replay_trace(tlbs, true, &args.itb_trace, &args.out_itb_trace);
    let dtb = replay_trace(tlbs, false, &args.dtb_trace, &args.out_dtb_trace);

    let writer = File::create(&args.out_state).expect("cannot create out state");
    let mut encoder = zstd::Encoder::new(writer, 3).unwrap();
    serde_json::to_writer(&mut encoder, &cores).unwrap();
    encoder.finish().unwrap();

    print_counts("itb", &itb);
    print_counts("dtb", &dtb);
}
