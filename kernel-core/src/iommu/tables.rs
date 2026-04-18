//! ACPI DMAR (Intel) and IVRS (AMD) structure decoders.
//!
//! Implementation lands in Phase 55a Track A.0. This commit introduces
//! only the failing test suite; the decoder types and functions land
//! in the next commit.

// Scaffolding — contents added by Track A.0.

// ---------------------------------------------------------------------------
// Tests — committed first, expected to fail to compile against the empty
// module so the TDD ordering is visible in `git log --follow`.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use proptest::prelude::*;

    // -----------------------------------------------------------------
    // Test helpers — synthesize blobs the decoders must accept.
    // -----------------------------------------------------------------

    fn push_u16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }
    fn push_u64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn make_sdt_header(signature: &[u8; 4], revision: u8) -> Vec<u8> {
        let mut hdr = Vec::with_capacity(ACPI_SDT_HEADER_LEN);
        hdr.extend_from_slice(signature);
        push_u32(&mut hdr, 0);
        hdr.push(revision);
        hdr.push(0);
        hdr.extend_from_slice(b"M3OSTS");
        hdr.extend_from_slice(b"M3TABLE1");
        push_u32(&mut hdr, 1);
        push_u32(&mut hdr, 0x4D33_4F53);
        push_u32(&mut hdr, 1);
        debug_assert_eq!(hdr.len(), ACPI_SDT_HEADER_LEN);
        hdr
    }

    fn finalize_table(bytes: &mut [u8]) {
        let len = bytes.len() as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        bytes[9] = 0;
        let sum: u8 = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        bytes[9] = 0u8.wrapping_sub(sum);
    }

    fn make_dmar_prefix(revision: u8, host_addr_width: u8, flags: u8) -> Vec<u8> {
        let mut out = make_sdt_header(b"DMAR", revision);
        out.push(host_addr_width);
        out.push(flags);
        out.extend_from_slice(&[0u8; 10]);
        out
    }

    fn make_ivrs_prefix(revision: u8, iv_info: u32) -> Vec<u8> {
        let mut out = make_sdt_header(b"IVRS", revision);
        push_u32(&mut out, iv_info);
        out.extend_from_slice(&[0u8; 8]);
        out
    }

    fn push_device_scope(out: &mut Vec<u8>, scope_type: u8, bus: u8, path: &[(u8, u8)]) {
        let length = 6 + path.len() * 2;
        out.push(scope_type);
        out.push(length as u8);
        out.push(0);
        out.push(0);
        out.push(0);
        out.push(bus);
        for &(dev, func) in path {
            out.push(dev);
            out.push(func);
        }
    }

    fn make_drhd(flags: u8, segment: u16, base: u64, scopes: &[(u8, u8, &[(u8, u8)])]) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_DRHD);
        push_u16(&mut out, 0);
        out.push(flags);
        out.push(0);
        push_u16(&mut out, segment);
        push_u64(&mut out, base);
        for (stype, bus, path) in scopes {
            push_device_scope(&mut out, *stype, *bus, path);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn make_rmrr(
        segment: u16,
        base_addr: u64,
        limit_addr: u64,
        scopes: &[(u8, u8, &[(u8, u8)])],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_RMRR);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, segment);
        push_u64(&mut out, base_addr);
        push_u64(&mut out, limit_addr);
        for (stype, bus, path) in scopes {
            push_device_scope(&mut out, *stype, *bus, path);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn make_atsr(flags: u8, segment: u16, scopes: &[(u8, u8, &[(u8, u8)])]) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_ATSR);
        push_u16(&mut out, 0);
        out.push(flags);
        out.push(0);
        push_u16(&mut out, segment);
        for (stype, bus, path) in scopes {
            push_device_scope(&mut out, *stype, *bus, path);
        }
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn make_rhsa(register_base: u64, proximity: u32) -> Vec<u8> {
        let mut out = Vec::new();
        push_u16(&mut out, DMAR_TYPE_RHSA);
        push_u16(&mut out, 20);
        push_u32(&mut out, 0);
        push_u64(&mut out, register_base);
        push_u32(&mut out, proximity);
        out
    }

    fn make_ivhd(
        block_type: u8,
        flags: u8,
        device_id: u16,
        cap: u16,
        base: u64,
        segment: u16,
        info: u16,
        feature: u32,
        device_entries: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(block_type);
        out.push(flags);
        push_u16(&mut out, 0);
        push_u16(&mut out, device_id);
        push_u16(&mut out, cap);
        push_u64(&mut out, base);
        push_u16(&mut out, segment);
        push_u16(&mut out, info);
        push_u32(&mut out, feature);
        out.extend_from_slice(device_entries);
        let len = out.len() as u16;
        out[2..4].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn push_ivhd_short(out: &mut Vec<u8>, entry_type: u8, device_id: u16, data_setting: u8) {
        out.push(entry_type);
        push_u16(out, device_id);
        out.push(data_setting);
    }

    fn push_ivhd_alias(
        out: &mut Vec<u8>,
        entry_type: u8,
        device_id: u16,
        data_setting: u8,
        alias: u16,
    ) {
        out.push(entry_type);
        push_u16(out, device_id);
        out.push(data_setting);
        out.push(0);
        push_u16(out, alias);
        out.push(0);
    }

    // -----------------------------------------------------------------
    // DMAR unit tests
    // -----------------------------------------------------------------

    #[test]
    fn dmar_decode_empty_table_no_subtables() {
        let mut bytes = make_dmar_prefix(1, 39, 0);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).expect("empty DMAR decodes");
        assert!(tables.header.is_some());
        assert!(tables.drhds.is_empty());
        assert!(tables.rmrrs.is_empty());
        assert!(tables.atsrs.is_empty());
        assert!(tables.rhsas.is_empty());
        assert_eq!(tables.unknown_subtables, 0);
        let hdr = tables.header.unwrap();
        assert_eq!(&hdr.signature, b"DMAR");
        assert_eq!(hdr.host_addr_width, 39);
        assert_eq!(hdr.revision, 1);
    }

    #[test]
    fn dmar_decode_drhd_without_scopes() {
        let mut bytes = make_dmar_prefix(1, 48, 1);
        let drhd = make_drhd(0x01, 0, 0xFED9_0000, &[]);
        bytes.extend_from_slice(&drhd);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).expect("decode DRHD");
        assert_eq!(tables.drhds.len(), 1);
        let entry = &tables.drhds[0];
        assert_eq!(entry.flags, 0x01);
        assert_eq!(entry.segment, 0);
        assert_eq!(entry.register_base_address, 0xFED9_0000);
        assert!(entry.device_scopes.is_empty());
    }

    #[test]
    fn dmar_decode_drhd_with_scopes() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let scopes: &[(u8, u8, &[(u8, u8)])] = &[(1, 0x10, &[(0x1f, 0x02), (0x00, 0x00)])];
        let drhd = make_drhd(0, 1, 0x1000_0000, scopes);
        bytes.extend_from_slice(&drhd);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.drhds.len(), 1);
        let scope = &tables.drhds[0].device_scopes[0];
        assert_eq!(scope.scope_type, 1);
        assert_eq!(scope.start_bus, 0x10);
        assert_eq!(scope.path, vec![(0x1f, 0x02), (0x00, 0x00)]);
    }

    #[test]
    fn dmar_decode_rmrr() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let rmrr = make_rmrr(0, 0x8000_0000, 0x8FFF_FFFF, &[(1, 0x00, &[(0x02, 0x00)])]);
        bytes.extend_from_slice(&rmrr);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.rmrrs.len(), 1);
        assert_eq!(tables.rmrrs[0].base_addr, 0x8000_0000);
        assert_eq!(tables.rmrrs[0].limit_addr, 0x8FFF_FFFF);
        assert_eq!(tables.rmrrs[0].device_scopes.len(), 1);
    }

    #[test]
    fn dmar_decode_atsr() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let atsr = make_atsr(0x00, 0, &[(1, 0x00, &[])]);
        bytes.extend_from_slice(&atsr);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.atsrs.len(), 1);
        assert_eq!(tables.atsrs[0].device_scopes.len(), 1);
    }

    #[test]
    fn dmar_decode_rhsa() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let rhsa = make_rhsa(0xFED9_0000, 2);
        bytes.extend_from_slice(&rhsa);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.rhsas.len(), 1);
        assert_eq!(tables.rhsas[0].register_base_address, 0xFED9_0000);
        assert_eq!(tables.rhsas[0].proximity_domain, 2);
    }

    #[test]
    fn dmar_unknown_subtable_is_counted_not_fatal() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let mut unknown = Vec::new();
        push_u16(&mut unknown, 4);
        push_u16(&mut unknown, 8);
        unknown.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&unknown);
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.unknown_subtables, 1);
        assert!(tables.drhds.is_empty());
    }

    #[test]
    fn dmar_mixed_subtables_are_all_returned() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        bytes.extend_from_slice(&make_drhd(0, 0, 0xFED9_0000, &[]));
        bytes.extend_from_slice(&make_rmrr(0, 0x9000_0000, 0x9FFF_FFFF, &[]));
        bytes.extend_from_slice(&make_atsr(0, 0, &[]));
        bytes.extend_from_slice(&make_rhsa(0xFED9_0000, 0));
        finalize_table(&mut bytes);
        let tables = decode_dmar(&bytes).unwrap();
        assert_eq!(tables.drhds.len(), 1);
        assert_eq!(tables.rmrrs.len(), 1);
        assert_eq!(tables.atsrs.len(), 1);
        assert_eq!(tables.rhsas.len(), 1);
    }

    #[test]
    fn dmar_truncated_header_returns_error() {
        let bytes = [0u8; 10];
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::TruncatedHeader
        );
    }

    #[test]
    fn dmar_invalid_checksum_returns_error() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        finalize_table(&mut bytes);
        bytes[9] = bytes[9].wrapping_add(1);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::InvalidChecksum
        );
    }

    #[test]
    fn dmar_unknown_revision_returns_error() {
        let mut bytes = make_dmar_prefix(0, 48, 0);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::UnknownRevision
        );
    }

    #[test]
    fn dmar_truncated_subtable_returns_error() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        push_u16(&mut bytes, DMAR_TYPE_DRHD);
        push_u16(&mut bytes, 0xFFFF);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::TruncatedSubTable
        );
    }

    #[test]
    fn dmar_invalid_device_scope_returns_error() {
        let mut bytes = make_dmar_prefix(1, 48, 0);
        let mut drhd = Vec::new();
        push_u16(&mut drhd, DMAR_TYPE_DRHD);
        push_u16(&mut drhd, 0);
        drhd.extend_from_slice(&[0u8; 12]);
        drhd.extend_from_slice(&[1u8, 3u8, 0u8]);
        let len = drhd.len() as u16;
        drhd[2..4].copy_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&drhd);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_dmar(&bytes).unwrap_err(),
            DmarParseError::InvalidDeviceScope
        );
    }

    // -----------------------------------------------------------------
    // IVRS unit tests
    // -----------------------------------------------------------------

    #[test]
    fn ivrs_decode_empty_table_no_blocks() {
        let mut bytes = make_ivrs_prefix(1, 0x1010);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).expect("empty IVRS decodes");
        assert!(tables.header.is_some());
        assert!(tables.ivhd_blocks.is_empty());
        assert_eq!(tables.unknown_blocks, 0);
        let hdr = tables.header.unwrap();
        assert_eq!(&hdr.signature, b"IVRS");
        assert_eq!(hdr.iv_info, 0x1010);
    }

    #[test]
    fn ivrs_decode_ivhd_10h_with_select() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        push_ivhd_short(&mut entries, IVHD_ENTRY_SELECT, 0x0030, 0);
        let block = make_ivhd(IVHD_TYPE_10H, 0x40, 0x0018, 0x40, 0xFEB8_0000, 0, 0, 0, &entries);
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivhd_blocks.len(), 1);
        let blk = &tables.ivhd_blocks[0];
        assert_eq!(blk.block_type, IVHD_TYPE_10H);
        assert_eq!(blk.flags, 0x40);
        assert_eq!(blk.iommu_base_address, 0xFEB8_0000);
        assert_eq!(blk.device_entries.len(), 1);
        assert!(matches!(
            blk.device_entries[0],
            IvhdDeviceEntry::Select {
                device_id: 0x0030,
                data_setting: 0
            }
        ));
    }

    #[test]
    fn ivrs_decode_ivhd_11h_with_range() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        push_ivhd_short(&mut entries, IVHD_ENTRY_START_RANGE, 0x0100, 0);
        push_ivhd_short(&mut entries, IVHD_ENTRY_END_RANGE, 0x01FF, 0);
        let block = make_ivhd(IVHD_TYPE_11H, 0, 0x0018, 0x40, 0xFEB8_0000, 0, 0, 0, &entries);
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivhd_blocks.len(), 1);
        let blk = &tables.ivhd_blocks[0];
        assert_eq!(blk.block_type, IVHD_TYPE_11H);
        assert_eq!(blk.device_entries.len(), 2);
        assert!(matches!(
            blk.device_entries[0],
            IvhdDeviceEntry::StartRange {
                device_id: 0x0100,
                ..
            }
        ));
        assert!(matches!(
            blk.device_entries[1],
            IvhdDeviceEntry::EndRange {
                device_id: 0x01FF,
                ..
            }
        ));
    }

    #[test]
    fn ivrs_decode_ivhd_40h_with_alias() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        push_ivhd_alias(&mut entries, IVHD_ENTRY_ALIAS_SELECT, 0x0200, 0, 0x0210);
        push_ivhd_alias(&mut entries, IVHD_ENTRY_ALIAS_START_RANGE, 0x0300, 0, 0x0310);
        let block = make_ivhd(
            IVHD_TYPE_40H,
            0x80,
            0x0018,
            0x40,
            0xFEB8_0000,
            0,
            0,
            0xDEAD_BEEF,
            &entries,
        );
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.ivhd_blocks.len(), 1);
        let blk = &tables.ivhd_blocks[0];
        assert_eq!(blk.block_type, IVHD_TYPE_40H);
        assert_eq!(blk.iommu_feature_info, 0xDEAD_BEEF);
        assert_eq!(blk.device_entries.len(), 2);
        match blk.device_entries[0] {
            IvhdDeviceEntry::AliasSelect {
                device_id,
                alias_device_id,
                ..
            } => {
                assert_eq!(device_id, 0x0200);
                assert_eq!(alias_device_id, 0x0210);
            }
            _ => panic!("expected AliasSelect"),
        }
        match blk.device_entries[1] {
            IvhdDeviceEntry::AliasStartRange {
                device_id,
                alias_device_id,
                ..
            } => {
                assert_eq!(device_id, 0x0300);
                assert_eq!(alias_device_id, 0x0310);
            }
            _ => panic!("expected AliasStartRange"),
        }
    }

    #[test]
    fn ivrs_unknown_block_is_counted() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut unknown = Vec::new();
        unknown.push(0x20);
        unknown.push(0);
        push_u16(&mut unknown, 8);
        unknown.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(&unknown);
        finalize_table(&mut bytes);
        let tables = decode_ivrs(&bytes).unwrap();
        assert_eq!(tables.unknown_blocks, 1);
        assert!(tables.ivhd_blocks.is_empty());
    }

    #[test]
    fn ivrs_truncated_header_returns_error() {
        let bytes = [0u8; 16];
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::TruncatedHeader
        );
    }

    #[test]
    fn ivrs_invalid_checksum_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        finalize_table(&mut bytes);
        bytes[9] = bytes[9].wrapping_add(1);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::InvalidChecksum
        );
    }

    #[test]
    fn ivrs_unknown_revision_returns_error() {
        let mut bytes = make_ivrs_prefix(0, 0);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::UnknownRevision
        );
    }

    #[test]
    fn ivrs_truncated_block_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        bytes.push(IVHD_TYPE_10H);
        bytes.push(0);
        push_u16(&mut bytes, 0xFFFF);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::TruncatedSubTable
        );
    }

    #[test]
    fn ivrs_invalid_device_entry_returns_error() {
        let mut bytes = make_ivrs_prefix(1, 0);
        let mut entries = Vec::new();
        entries.extend_from_slice(&[5u8, 0u8, 0u8, 0u8]);
        let block = make_ivhd(IVHD_TYPE_10H, 0, 0, 0, 0, 0, 0, 0, &entries);
        bytes.extend_from_slice(&block);
        finalize_table(&mut bytes);
        assert_eq!(
            decode_ivrs(&bytes).unwrap_err(),
            IvrsParseError::InvalidDeviceScope
        );
    }

    // -----------------------------------------------------------------
    // Property tests — no panics, bounded output on arbitrary inputs.
    // -----------------------------------------------------------------

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn decode_dmar_is_panic_free_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let result = decode_dmar(&bytes);
            if let Ok(tables) = result {
                let max_items = bytes.len() / SUBTABLE_HEADER_LEN + 1;
                let total = tables.drhds.len()
                    + tables.rmrrs.len()
                    + tables.atsrs.len()
                    + tables.rhsas.len()
                    + tables.unknown_subtables as usize;
                prop_assert!(total <= max_items);
            }
        }

        #[test]
        fn decode_ivrs_is_panic_free_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let result = decode_ivrs(&bytes);
            if let Ok(tables) = result {
                let max_items = bytes.len() / 4 + 1;
                prop_assert!(tables.ivhd_blocks.len() + tables.unknown_blocks as usize <= max_items);
            }
        }
    }
}
