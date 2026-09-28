// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Regis Boudinot
#![no_main]
use libfuzzer_sys::fuzz_target;
use zimz_core::source::MemorySource;

fuzz_target!(|data: &[u8]| {
    let Ok(archive) = zimz_core::Archive::from_source(Box::new(MemorySource::new(data.to_vec()))) else {
        return;
    };
    let _ = archive.main_entry();
    let _ = archive.metadata("Title");
    for i in 0..archive.entry_count().min(64) {
        if let Ok(e) = archive.entry(i) {
            let _ = archive.item_data(&e);
        }
    }
    let _ = archive.verify_checksum();
});
