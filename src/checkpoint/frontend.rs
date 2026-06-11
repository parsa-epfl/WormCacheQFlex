use crate::components::bp::fetch::{
    bbl_btb,
    btb::BTBEntry,
    tage::{self, *},
};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::FlexusParameter;

#[derive(Serialize, Deserialize)]
struct FlexusBTBEntry {
    #[serde(rename = "PC")]
    pc: u64,
    target: u64,
    #[serde(rename = "type")]
    type_: u64,
    ts: u64, // for debugging
    bbl_bytes: u64,
}

#[derive(Serialize, Deserialize)]
struct BTBProxy {
    array: Vec<Vec<BTBEntry>>,
}

fn bbl_bytes_by_pc(
    restore_export: Option<RestoreExportProxy>,
) -> FxHashMap<u64, u64> {
    let mut out = FxHashMap::default();

    let Some(restore_export) = restore_export else {
        return out;
    };

    for set in restore_export.bbl_btb.array {
        for entry in set {
            if entry.ts == 0 {
                continue;
            }
            out.insert(entry.branch_pc, entry.bbl_bytes);
        }
    }

    out
}

fn serialize_a_btb(
    btb_proxy: BTBProxy,
    bbl_bytes_by_pc: &FxHashMap<u64, u64>,
    flexus_configuration: &FlexusParameter,
) -> Vec<Vec<FlexusBTBEntry>> {
    assert!(btb_proxy.array.len() % flexus_configuration.btb_sets == 0);

    if flexus_configuration.no_resizing {
        assert!(btb_proxy.array.len() == flexus_configuration.btb_sets);
    }

    let mut serialized_btb = Vec::new();

    // Step 0: Initialize the serialized BTB.
    for _ in 0..flexus_configuration.btb_sets {
        serialized_btb.push(Vec::new());
    }

    // Step 1: Merge sets.
    for (old_set_idx, mut old_set) in btb_proxy.array.into_iter().enumerate() {
        let new_set_idx = old_set_idx % flexus_configuration.btb_sets;
        old_set.retain(|entry| entry.ts != 0); // Filter out invalid entries.
        serialized_btb[new_set_idx].append(&mut old_set);
    }

    // As a sanity check, all entries's PC should be unique.
    let mut pc_set = FxHashSet::default();
    for set in serialized_btb.iter() {
        for entry in set.iter() {
            assert!(!pc_set.contains(&entry.tag));
            pc_set.insert(entry.tag);
        }
    }

    // Step 2: Apply LRU associativity.
    for set in serialized_btb.iter_mut() {
        set.sort_by_key(|entry| entry.ts);
        set.reverse();

        if flexus_configuration.no_resizing {
            assert!(
                set.len() <= flexus_configuration.btb_associativity,
                "BTB set is too large",
            );
        }

        set.truncate(flexus_configuration.btb_associativity);
    }

    // Step 3: Serialize the BTB.
    let mut serialized_btb_json = Vec::new();
    for set in serialized_btb.iter() {
        let mut serialized_set = Vec::new();
        for entry in set.iter().rev() {
            serialized_set.push(FlexusBTBEntry {
                pc: entry.tag,
                target: entry.target,
                type_: entry.branch_type as u64,
                ts: entry.ts,
                bbl_bytes: bbl_bytes_by_pc.get(&entry.tag).copied().unwrap_or(0),
            });
        }
        serialized_btb_json.push(serialized_set);
    }

    serialized_btb_json
}

#[derive(Serialize, Deserialize)]
struct FlexusTAGEPredictorState {
    #[serde(rename = "PWIN")]
    pub pwin: i32,
    #[serde(rename = "TICK")]
    pub tick: i32,
    #[serde(rename = "SEED")]
    pub seed: i32,
    #[serde(rename = "PHIST")]
    pub phist: i32,
    #[serde(rename = "GHIST")]
    pub ghist: Vec<bool>,

    #[serde(rename = "LOGB")]
    pub logb: usize,
    #[serde(rename = "NHIST")]
    pub nhist: usize,
    #[serde(rename = "LOGG")]
    pub logg: usize,
    #[serde(rename = "TBITS")]
    pub tbits: usize,
    #[serde(rename = "MAXHIST")]
    pub maxhist: usize,
    #[serde(rename = "MINHIST")]
    pub minhist: usize,
    #[serde(rename = "CBITS")]
    pub cbits: usize,

    pub btable: Vec<TAGEBiModalEntry>,
    pub gtable: Vec<Vec<TAGEGlobalTableEntry>>,

    pub ch_i: Vec<FoldedHistory>,
    pub ch_t: Vec<Vec<FoldedHistory>>,

    pub m: Vec<usize>,
}

fn serialize_a_tage(
    tage: crate::components::bp::fetch::tage::TAGEPredictor,
) -> FlexusTAGEPredictorState {
    FlexusTAGEPredictorState {
        pwin: 0,
        tick: tage.tick,
        seed: tage.seed,
        phist: tage.phist,
        ghist: tage.ghist.iter().copied().collect(),

        logb: LOGB,
        nhist: NHIST,
        logg: LOGG,
        tbits: TBITS,
        maxhist: MAXHIST,
        minhist: MINHIST,
        cbits: CBITS,

        btable: tage.btable.to_vec(),
        gtable: tage.gtable.iter().map(|x| x.to_vec()).collect(),

        ch_i: tage.ch_i.to_vec(),
        ch_t: tage.ch_t.iter().map(|x| x.to_vec()).collect(),

        m: HISTORIES.to_vec(),
    }
}

#[derive(Serialize, Deserialize)]
struct RestoreExportProxy {
    bbl_btb: bbl_btb::BblBTB<{ crate::parameter::BTB_SET }, { crate::parameter::BTB_ASSO }>,
}

#[derive(Serialize, Deserialize)]
struct PerCoreFetchUnitProxy {
    #[serde(alias = "pc_btb")]
    btb: BTBProxy,
    tage: tage::TAGEPredictor,
    #[serde(default)]
    restore_export: Option<RestoreExportProxy>,
}

#[derive(Serialize, Deserialize)]
struct FlexusFetchUnit {
    private_units: Vec<PerCoreFetchUnitProxy>,
}

impl FlexusFetchUnit {
    pub fn export(self, folder_name: &String, flexus_configuration: &FlexusParameter) {
        for (core_id, unit) in self.private_units.into_iter().enumerate() {
            let file_name = format!("{}/{:03}-bpred.json", folder_name, core_id);
            let file = std::fs::File::create(&file_name).unwrap();
            serde_json::to_writer(
                file,
                &json!({
                    "btb": serialize_a_btb(
                        unit.btb,
                        &bbl_bytes_by_pc(unit.restore_export),
                        flexus_configuration,
                    ),
                    "tage": serialize_a_tage(unit.tage),
                }),
            )
            .unwrap();
            println!("Core {}'s fetch unit is exported to {}", core_id, file_name);
        }
    }
}
pub fn process_frontend(
    checkpoint_folder: &String,
    flexus_configuration: &FlexusParameter,
    output_folder: &String,
) {
    // find the frontend checkpoint.
    let frontend_checkpoint = std::fs::read_dir(checkpoint_folder)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            let file_name = entry.file_name().into_string().unwrap();
            if file_name.ends_with("fetch.json.zstd") {
                Some(file_name)
            } else {
                None
            }
        })
        .collect::<Vec<String>>();

    assert_eq!(frontend_checkpoint.len(), 1);
    println!(
        "Frontend checkpoint is detected. Filename: {}",
        frontend_checkpoint[0]
    );

    let file =
        std::fs::File::open(format!("{}/{}", checkpoint_folder, frontend_checkpoint[0])).unwrap();

    let file = zstd::Decoder::new(file).unwrap();

    let unit: FlexusFetchUnit = serde_json::from_reader(file).unwrap();

    unit.export(output_folder, flexus_configuration);
}
