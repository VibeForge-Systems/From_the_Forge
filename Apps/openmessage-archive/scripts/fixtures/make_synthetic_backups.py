#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 VibeForge Systems, Inc. and OpenMessage Archive contributors
"""Build the SYNTHETIC iPhone backup fixtures for OpenMessage Archive.

Every value written here is invented. No real device, person, message or
backup is used. The output is test input for crates/oma-backup.

Written from the format description in docs/supported-formats.md (TLV
keybag, two-stage PBKDF2, RFC 3394 key wrap, AES-256-CBC with PKCS#7,
NSKeyedArchiver MBFile records, Manifest.db layout). It does not copy code
from any GPL, LGPL or CC BY-SA source.

Two modes:

  make_synthetic_backups.py OUT_DIR [--seed N]
      Writes the whole catalogue (every fixture named in the M2 spec) into
      OUT_DIR/<name>/SYNTHETIC0-0000000000000000/, the expected plaintexts
      into OUT_DIR/<name>.expected/, and OUT_DIR/SHA256SUMS.

  make_synthetic_backups.py OUT_DIR --single NAME [flags...]
      Writes one backup, built from the flags below, into
      OUT_DIR/NAME/SYNTHETIC0-0000000000000000/. For experiments only.

For a given --seed and the same Python sqlite3 build the output is
reproducible. Across machines it is not claimed to be byte-identical,
because SQLite file bytes depend on the SQLite version.

Dependencies: the standard library plus `cryptography` (see
requirements.txt). Run with `python -I`.
"""
import argparse
import datetime as dt
import hashlib
import os
import plistlib
import random
import shutil
import sqlite3
import struct
import sys
import tempfile

from cryptography.hazmat.primitives import padding as sym_padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.keywrap import aes_key_wrap

FIXED_DATE = dt.datetime(2026, 1, 2, 3, 4, 5)  # naive datetimes are UTC in plistlib
SYNTHETIC_UDID = "SYNTHETIC0-0000000000000000"  # also the backup folder name
DEVICE_NAME = "Synthetic Test Phone"
PRODUCT_TYPE = "iPhone0,0"
DEFAULT_PASSWORD = "synthetic-test-password"
UNICODE_PASSWORD = "synthetic-pässwörd-ключ-\U0001F511"
SNAPSHOT_SENTINEL = "synthetic-uploading"
MANIFEST_CLASS = 4  # protection class used for the ManifestKey (a synthetic choice)
CLASSES = tuple(range(1, 12))
# Iteration counts the keybag can declare without the fixture being derivable;
# above these, keys are wrapped with the default counts and the keybag only
# *declares* the hostile value (the backup is meant to be refused).
DERIVABLE_DPIC = 20_000_000
DERIVABLE_ITER = 1_000_000
DEFAULT_DPIC = 1000
DEFAULT_ITER = 10


# --------------------------------------------------------------------------
# Primitives
# --------------------------------------------------------------------------

def tlv(tag: bytes, value) -> bytes:
    if isinstance(value, int):
        value = struct.pack(">I", value)
    assert len(tag) == 4
    return tag + struct.pack(">I", len(value)) + value


def cbc_encrypt(key: bytes, data: bytes) -> bytes:
    padder = sym_padding.PKCS7(128).padder()
    padded = padder.update(data) + padder.finalize()
    enc = Cipher(algorithms.AES(key), modes.CBC(b"\x00" * 16)).encryptor()
    return enc.update(padded) + enc.finalize()


def cbc_padding_valid(key: bytes, ct: bytes) -> bool:
    dec = Cipher(algorithms.AES(key), modes.CBC(b"\x00" * 16)).decryptor()
    pt = dec.update(ct) + dec.finalize()
    unpadder = sym_padding.PKCS7(128).unpadder()
    try:
        unpadder.update(pt) + unpadder.finalize()
        return True
    except ValueError:
        return False


def file_id(domain: str, rel: str) -> str:
    return hashlib.sha1(f"{domain}-{rel}".encode("utf-8")).hexdigest()


def xml_uids(value):
    """plistlib's XML writer has no UID type; NSKeyedArchiver's XML form is
    a {"CF$UID": n} dictionary."""
    if isinstance(value, plistlib.UID):
        return {"CF$UID": value.data}
    if isinstance(value, dict):
        return {k: xml_uids(v) for k, v in value.items()}
    if isinstance(value, list):
        return [xml_uids(v) for v in value]
    return value


def write_plist(path: str, value, fmt) -> None:
    with open(path, "wb") as f:
        plistlib.dump(value, f, fmt=fmt)


def nested_pair_bomb(levels: int):
    """Each level is one array object referenced twice by the next, so the
    file is tiny and a naive reader expands it to 2**levels nodes."""
    x = [0]
    for _ in range(levels):
        x = [x, x]
    return x


# --------------------------------------------------------------------------
# Content
# --------------------------------------------------------------------------

def synthetic_sms_db(path: str) -> None:
    """A tiny SQLite file standing in for sms.db. Placeholder strings only."""
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE synthetic_marker (id INTEGER PRIMARY KEY, note TEXT)")
    con.execute("INSERT INTO synthetic_marker (note) VALUES ('synthetic fixture, not real data')")
    con.commit()
    con.close()


ATTACHMENT_REL = "Library/SMS/Attachments/00/00/SYNTH-0000/note.txt"
ATTACHMENT_BYTES = b"synthetic attachment payload\n" * 3


def mbfile(o, *, size, mode, prot, wrapped_key, target=None, inode=1000, pad=0, size_override=None):
    """An NSKeyedArchiver plist holding one MBFile object."""
    UID = plistlib.UID
    objects = ["$null"]
    ts = int(FIXED_DATE.replace(tzinfo=dt.timezone.utc).timestamp())
    root = {
        "Birth": ts,
        "LastModified": ts,
        "LastStatusChange": ts,
        "Mode": mode,
        "UserID": 501,
        "GroupID": 501,
        "InodeNumber": inode,
        "Size": size if size_override is None else size_override,
        "Flags": 0,
        "ProtectionClass": prot,
    }
    if pad:
        root["SyntheticPadding"] = b"\x00" * pad
    objects.append(root)  # index 1, but found through $top.root
    if wrapped_key is not None:
        blob = struct.pack("<I", prot) + wrapped_key
        if o.enc_key_shape == "nsdata":
            objects.append({"NS.data": blob, "$class": UID(0)})
            key_index = len(objects) - 1
            root["EncryptionKey"] = UID(key_index)
            objects.append({"$classname": "NSMutableData", "$classes": ["NSMutableData", "NSData", "NSObject"]})
            objects[key_index]["$class"] = UID(len(objects) - 1)
        elif o.enc_key_shape == "uid-data":
            objects.append(blob)
            root["EncryptionKey"] = UID(len(objects) - 1)
        elif o.enc_key_shape == "inline":
            root["EncryptionKey"] = blob
        else:
            raise ValueError(o.enc_key_shape)
    if target is not None:
        objects.append(target)
        root["Target"] = UID(len(objects) - 1)
    objects.append({"$classname": "MBFile", "$classes": ["MBFile", "NSObject"]})
    root["$class"] = UID(len(objects) - 1)
    archive = {
        "$version": 100000,
        "$archiver": "NSKeyedArchiver",
        "$top": {"root": UID(1)},
        "$objects": objects,
    }
    if o.xml_mbfile:
        return plistlib.dumps(xml_uids(archive), fmt=plistlib.FMT_XML)
    return plistlib.dumps(archive, fmt=plistlib.FMT_BINARY)


def mbfile_bomb() -> bytes:
    archive = {
        "$version": 100000,
        "$archiver": "NSKeyedArchiver",
        "$top": {"root": plistlib.UID(1)},
        "$objects": ["$null", {"Size": 1, "Bomb": nested_pair_bomb(25)}],
    }
    return plistlib.dumps(archive, fmt=plistlib.FMT_BINARY)


# --------------------------------------------------------------------------
# One backup
# --------------------------------------------------------------------------

def build_keybag(o, rb, password: str):
    """Returns (keybag bytes, class keys)."""
    dpic_declared, iter_declared = o.dpic, o.iter
    dpic = dpic_declared if 1 <= dpic_declared <= DERIVABLE_DPIC else DEFAULT_DPIC
    iters = iter_declared if 1 <= iter_declared <= DERIVABLE_ITER else DEFAULT_ITER
    salt, dpsl = rb(20), rb(20)
    stage1 = hashlib.pbkdf2_hmac("sha256", password.encode("utf-8"), dpsl, dpic, 32)
    pass_key = hashlib.pbkdf2_hmac("sha1", stage1, salt, iters, 32)

    kb = b""
    kb += tlv(b"VERS", 4)
    kb += tlv(b"TYPE", o.keybag_type)
    kb += tlv(b"UUID", rb(16))  # first UUID: the keybag's own
    kb += tlv(b"HMCK", rb(40))
    kb += tlv(b"WRAP", 0)
    kb += tlv(b"SALT", salt)
    kb += tlv(b"ITER", iter_declared)
    kb += tlv(b"DPWT", 1)
    kb += tlv(b"DPIC", dpic_declared)
    kb += tlv(b"DPSL", dpsl)
    class_keys = {}
    wrap = 1 if o.wrap_device_only else 2
    for clas in CLASSES:
        k = rb(32)
        class_keys[clas] = k
        kb += tlv(b"UUID", rb(16))  # each later UUID starts a class record
        kb += tlv(b"CLAS", clas)
        kb += tlv(b"WRAP", wrap)
        kb += tlv(b"KTYP", 0)
        kb += tlv(b"WPKY", aes_key_wrap(pass_key, k))
    if o.clas_unused is not None:
        kb += tlv(b"UUID", rb(16))
        kb += tlv(b"CLAS", o.clas_unused)
        kb += tlv(b"WRAP", 2)
        kb += tlv(b"KTYP", 0)
        kb += tlv(b"WPKY", aes_key_wrap(pass_key, rb(32)))
    if o.wpky_unused_len is not None:
        kb += tlv(b"UUID", rb(16))
        kb += tlv(b"CLAS", 12)
        kb += tlv(b"WRAP", 2)
        kb += tlv(b"KTYP", 0)
        kb += tlv(b"WPKY", rb(o.wpky_unused_len))
    if o.dup_clas:
        kb += tlv(b"UUID", rb(16))
        kb += tlv(b"CLAS", 3)
        kb += tlv(b"WRAP", 2)
        kb += tlv(b"KTYP", 0)
        kb += tlv(b"WPKY", aes_key_wrap(pass_key, rb(32)))
    if o.keybag_truncate:
        kb = kb[: -o.keybag_truncate]
    return kb, class_keys


def wal_checksum(data: bytes, s0: int, s1: int, big_endian: bool):
    """The WAL checksum from the SQLite file-format documentation: over
    32-bit words in pairs, s0 += x[i] + s1; s1 += x[i+1] + s0 (mod 2**32)."""
    fmt = ">" if big_endian else "<"
    for i in range(0, len(data), 8):
        x0, x1 = struct.unpack(fmt + "II", data[i:i + 8])
        s0 = (s0 + x0 + s1) & 0xFFFFFFFF
        s1 = (s1 + x1 + s0) & 0xFFFFFFFF
    return s0, s1


def normalize_wal(wal: bytes, salt1: int, salt2: int) -> bytes:
    """SQLite picks random WAL salts. Replace them with fixed values and
    recompute every checksum, so the fixture is reproducible."""
    magic, version, page_size, ckpt = struct.unpack(">IIII", wal[:16])
    assert magic in (0x377F0682, 0x377F0683), hex(magic)
    big = bool(magic & 1)
    header = struct.pack(">IIIIII", magic, version, page_size, ckpt, salt1, salt2)
    s0, s1 = wal_checksum(header, 0, 0, big)
    out = bytearray(header + struct.pack(">II", s0, s1))
    pos = 32
    frame = 24 + page_size
    while pos + frame <= len(wal):
        pgno, commit = struct.unpack(">II", wal[pos:pos + 8])
        page = wal[pos + 24:pos + frame]
        s0, s1 = wal_checksum(wal[pos:pos + 8] + page, s0, s1, big)
        out += struct.pack(">IIIIII", pgno, commit, salt1, salt2, s0, s1) + page
        pos += frame
    return bytes(out)


def build_manifest_db(o, path: str, entries, extra_rows):
    """Write Manifest.db. With --wal-manifest, the base rows are checkpointed
    into the main file and `extra_rows` are left in the -wal. Returns the
    -wal bytes, or None."""
    con = sqlite3.connect(path)
    if o.wal_manifest:
        con.execute("PRAGMA journal_mode=WAL")
    con.executescript(
        """
        CREATE TABLE Files (fileID TEXT PRIMARY KEY, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB);
        CREATE INDEX FilesDomainIdx ON Files(domain);
        CREATE INDEX FilesRelativePathIdx ON Files(relativePath);
        CREATE INDEX FilesFlagsIdx ON Files(flags);
        CREATE TABLE Properties (key TEXT PRIMARY KEY, value BLOB);
        """
    )
    con.executemany("INSERT INTO Files VALUES (?,?,?,?,?)", entries)
    con.commit()
    wal_bytes = None
    if o.wal_manifest:
        con.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        con.execute("PRAGMA wal_autocheckpoint=0")
        con.executemany("INSERT INTO Files VALUES (?,?,?,?,?)", extra_rows)
        con.commit()
        with open(path + "-wal", "rb") as f:
            wal_bytes = normalize_wal(f.read(), 0x5359_4E54, 0x4845_5449)  # "SYNT", "HETI"
        with open(path, "rb") as f:
            main_bytes = f.read()
        con.close()
        # Closing checkpoints the WAL into the main file; restore the
        # pre-close main file so the extra rows live only in the -wal.
        with open(path, "wb") as f:
            f.write(main_bytes)
        for suffix in ("-wal", "-shm"):
            if os.path.exists(path + suffix):
                os.remove(path + suffix)
        return wal_bytes
    con.close()
    return None


def build_backup(o, out_parent: str, rng: random.Random, expected_dir=None) -> None:
    rb = rng.randbytes
    out = os.path.join(out_parent, SYNTHETIC_UDID)
    if os.path.exists(out_parent):
        shutil.rmtree(out_parent)
    os.makedirs(out)
    encrypted = not o.unencrypted
    pfmt = plistlib.FMT_XML if o.xml_plists else plistlib.FMT_BINARY

    with tempfile.TemporaryDirectory() as work:
        # ---- Plists ------------------------------------------------------
        info = {
            "Build Version": "00A000",
            "Device Name": DEVICE_NAME,
            "Display Name": DEVICE_NAME,
            "Last Backup Date": FIXED_DATE,
            "Product Name": "Synthetic iPhone",
            "Product Type": PRODUCT_TYPE,
            "Product Version": "18.0",
            "Unique Identifier": SYNTHETIC_UDID,
        }
        if o.plist_bomb == "info":
            info["Applications"] = {"synthetic.app": nested_pair_bomb(25)}
        write_plist(os.path.join(out, "Info.plist"), info, plistlib.FMT_XML if o.plist_bomb != "info" else plistlib.FMT_BINARY)

        if not o.no_status_plist:
            status = {
                "BackupState": "new",
                "Date": FIXED_DATE,
                "IsFullBackup": True,
                "SnapshotState": o.snapshot_state,
                "UUID": "00000000-0000-0000-0000-000000000000",
                "Version": "2.4" if o.legacy_mbdb else "3.3",
            }
            sfmt = pfmt
            if o.plist_bomb == "status":
                status["SyntheticBomb"] = nested_pair_bomb(25)
                sfmt = plistlib.FMT_BINARY
            write_plist(os.path.join(out, "Status.plist"), status, sfmt)
        if o.snapshot_dir:
            os.makedirs(os.path.join(out, "Snapshot"))
            with open(os.path.join(out, "Snapshot", "synthetic-placeholder"), "wb") as f:
                f.write(b"synthetic\n")

        lockdown = {
            "BuildVersion": "00A000",
            "DeviceName": DEVICE_NAME,
            "ProductType": PRODUCT_TYPE,
            "ProductVersion": "18.0",
        }

        if o.legacy_mbdb:
            # Only the signature that marks format 2.4: enough to be detected
            # and refused, never read.
            with open(os.path.join(out, "Manifest.mbdb"), "wb") as f:
                f.write(b"mbdb\x05\x00")
            write_plist(os.path.join(out, "Manifest.plist"),
                        {"IsEncrypted": False, "Version": "9.1", "Lockdown": lockdown}, pfmt)
            return

        password = o.password
        keybag, class_keys = build_keybag(o, rb, password)

        # ---- Content -----------------------------------------------------
        sms_plain = os.path.join(work, "sms.db")
        synthetic_sms_db(sms_plain)
        with open(sms_plain, "rb") as f:
            sms_bytes = f.read()

        base = [
            # (domain, relativePath, flags, payload, mode, prot, target)
            ("HomeDomain", "", 2, None, 0o40755, 0, None),
            ("HomeDomain", "Library", 2, None, 0o40755, 0, None),
            ("HomeDomain", "Library/SMS", 2, None, 0o40755, 0, None),
            ("HomeDomain", "Library/SMS/sms.db", 1, sms_bytes, 0o100644, 3, None),
            ("MediaDomain", ATTACHMENT_REL, 1, ATTACHMENT_BYTES, 0o100644, 3, None),
            ("HomeDomain", "Library/SMS/link-to-nowhere", 4, None, 0o120755, 0, "/synthetic/target"),
        ]
        if o.hostile_paths:
            for i, rel in enumerate(["../../escape", "/abs", "C:\\x", "a\\..\\..\\b", "What?.jpg"]):
                base.append(("MediaDomain", rel, 1, b"synthetic hostile-name payload %d\n" % i, 0o100644, 3, None))
        if o.fileid_mismatch:
            base.append(("HomeDomain", "Library/SMS/other.db", 1, b"synthetic alias payload\n", 0o100644, 3, None))

        rows = []
        blobs = {}
        for inode, (domain, rel, flags, payload, mode, prot, target) in enumerate(base, start=1000):
            fid = file_id(domain, rel)
            if o.fileid_mismatch and rel == "Library/SMS/other.db":
                fid = file_id("HomeDomain", "Library/SMS/alias.db")
            wrapped = None
            if flags == 1:
                if encrypted:
                    fkey = rb(32)
                    wrapped = aes_key_wrap(class_keys[prot], fkey)
                    data = cbc_encrypt(fkey, payload)
                    if o.flip_last_byte == "sms" and rel == "Library/SMS/sms.db":
                        data = flip_until_invalid(fkey, data)
                else:
                    data = payload
                omit = o.omit_sms_blob and rel == "Library/SMS/sms.db"
                if not omit:
                    blobs[fid] = data
            size_override = None
            if o.size_max and rel == ATTACHMENT_REL:
                size_override = 2 ** 64 - 1
            pad = o.mbfile_pad if rel == ATTACHMENT_REL else 0
            record = mbfile(o, size=len(payload) if payload else 0, mode=mode, prot=prot,
                            wrapped_key=wrapped, target=target, inode=inode, pad=pad,
                            size_override=size_override)
            rows.append((fid, domain, rel, flags, record))
        if o.hostile_paths:
            # Two fileIDs that are not 40 lowercase hex characters.
            rows.append(("../../xx", "HomeDomain", "bad-id-1", 1, None))
            rows.append((file_id("HomeDomain", "bad-id-2").upper(), "HomeDomain", "bad-id-2", 1, None))
        if o.plist_bomb == "mbfile":
            rows.append((file_id("HomeDomain", "Library/SMS/bomb"), "HomeDomain", "Library/SMS/bomb", 1, mbfile_bomb()))

        extra_rows = [
            (file_id("HomeDomain", rel), "HomeDomain", rel, 2,
             mbfile(o, size=0, mode=0o40755, prot=0, wrapped_key=None, inode=2000 + i))
            for i, rel in enumerate(["Library/Preferences", "Library/Caches"])
        ]

        for fid, data in blobs.items():
            d = os.path.join(out, fid[:2])
            os.makedirs(d, exist_ok=True)
            with open(os.path.join(d, fid), "wb") as f:
                f.write(data)

        manifest_plain = os.path.join(work, "Manifest.db")
        wal_bytes = build_manifest_db(o, manifest_plain, rows, extra_rows)
        with open(manifest_plain, "rb") as f:
            mdb_plain = f.read()

        mdb = mdb_plain
        manifest_key_blob = None
        if encrypted:
            mkey = rb(32)
            manifest_key_blob = struct.pack("<I", MANIFEST_CLASS) + aes_key_wrap(class_keys[MANIFEST_CLASS], mkey)
            if not o.plaintext_manifest_with_encrypted_flag:
                mdb = cbc_encrypt(mkey, mdb_plain)
                if o.flip_last_byte == "manifest":
                    mdb = flip_until_invalid(mkey, mdb)
                if wal_bytes is not None:
                    # Whether an encrypted backup's -wal is encrypted with the
                    # ManifestKey is UNVERIFIED; this models that hypothesis.
                    wal_bytes = cbc_encrypt(mkey, wal_bytes)
        with open(os.path.join(out, "Manifest.db"), "wb") as f:
            f.write(mdb)
        if wal_bytes is not None:
            with open(os.path.join(out, "Manifest.db-wal"), "wb") as f:
                f.write(wal_bytes)

        manifest = {
            "Version": "10.0",
            "Date": FIXED_DATE,
            "SystemDomainsVersion": "24.0",
            "WasPasscodeSet": False,
            "IsEncrypted": encrypted,
            "Lockdown": lockdown,
            "Applications": {},
        }
        if o.isencrypted_type == "string":
            manifest["IsEncrypted"] = "true" if encrypted else "false"
        if not o.no_keybag:
            manifest["BackupKeyBag"] = keybag
        if encrypted and not o.no_manifest_key:
            manifest["ManifestKey"] = manifest_key_blob
        mfmt = pfmt
        if o.plist_bomb == "manifest":
            manifest["Applications"] = {"synthetic.app": nested_pair_bomb(25)}
            mfmt = plistlib.FMT_BINARY
        write_plist(os.path.join(out, "Manifest.plist"), manifest, mfmt)

        if expected_dir is not None:
            os.makedirs(expected_dir, exist_ok=True)
            with open(os.path.join(expected_dir, "Manifest.db"), "wb") as f:
                f.write(mdb_plain)
            shutil.copyfile(sms_plain, os.path.join(expected_dir, "sms.db"))
            with open(os.path.join(expected_dir, "attachment.txt"), "wb") as f:
                f.write(ATTACHMENT_BYTES)


def flip_until_invalid(key: bytes, ct: bytes) -> bytes:
    """Change the last ciphertext byte so the padding is invalid. A single
    flip leaves valid padding with probability about 1/256, so try values
    until the padding check fails."""
    for x in range(1, 256):
        bad = ct[:-1] + bytes([ct[-1] ^ x])
        if not cbc_padding_valid(key, bad):
            return bad
    raise RuntimeError("could not invalidate padding")


# --------------------------------------------------------------------------
# Catalogue
# --------------------------------------------------------------------------

def defaults() -> dict:
    return dict(
        unencrypted=False, password=DEFAULT_PASSWORD, dpic=DEFAULT_DPIC, iter=DEFAULT_ITER,
        snapshot_state="finished", no_status_plist=False, snapshot_dir=False,
        omit_sms_blob=False, xml_plists=False, xml_mbfile=False, legacy_mbdb=False,
        keybag_truncate=0, wrap_device_only=False, no_manifest_key=False, no_keybag=False,
        isencrypted_type="bool", flip_last_byte=None, hostile_paths=False,
        fileid_mismatch=False, mbfile_pad=0,
        plaintext_manifest_with_encrypted_flag=False, keybag_type=1, clas_unused=None,
        wpky_unused_len=None, dup_clas=False, plist_bomb=None, size_max=False,
        wal_manifest=False, enc_key_shape="nsdata",
    )


PLAIN = dict(unencrypted=True)

# name -> (overrides, write expected plaintexts)
CATALOGUE = {
    "plain": (dict(PLAIN), True),
    # XML MBFile records are a Rust-only shape (UNVERIFIED that they occur);
    # enc-xml keeps binary records so the oracle can read it.
    "plain-xml": (dict(PLAIN, xml_plists=True, xml_mbfile=True), True),
    "plain-wal": (dict(PLAIN, wal_manifest=True), False),
    "plain-no-keybag": (dict(PLAIN, no_keybag=True), False),
    "enc": ({}, True),
    "enc-xml": (dict(xml_plists=True), True),
    "enc-wal": (dict(wal_manifest=True), False),
    "enc-unicode-pw": (dict(password=UNICODE_PASSWORD), True),
    "enc-slow": (dict(dpic=2_000_000), False),
    "enc-type2": (dict(keybag_type=2), False),
    "enc-clas-unused-0x10": (dict(clas_unused=0x10), False),
    "enc-wpky39-unused": (dict(wpky_unused_len=39), False),
    "enc-dup-clas": (dict(dup_clas=True), False),
    "enc-key-uid-data": (dict(enc_key_shape="uid-data"), True),
    "enc-key-inline": (dict(enc_key_shape="inline"), True),
    "incomplete": (dict(PLAIN, snapshot_state=SNAPSHOT_SENTINEL), False),
    "incomplete-no-status": (dict(PLAIN, no_status_plist=True), False),
    "snapshot-dir": (dict(PLAIN, snapshot_state=SNAPSHOT_SENTINEL, snapshot_dir=True), False),
    "legacy-mbdb": (dict(PLAIN, legacy_mbdb=True), False),
    "enc-no-manifestkey": (dict(no_manifest_key=True), False),
    "enc-no-keybag": (dict(no_keybag=True), False),
    "wrong-type-isencrypted": (dict(isencrypted_type="string"), False),
    "encrypted-plaintext-db": (dict(plaintext_manifest_with_encrypted_flag=True), False),
    "bomb-info": (dict(PLAIN, plist_bomb="info"), False),
    "bomb-status": (dict(PLAIN, plist_bomb="status"), False),
    "bomb-manifest": (dict(PLAIN, plist_bomb="manifest"), False),
    "bomb-mbfile": (dict(PLAIN, plist_bomb="mbfile"), False),
    "hostile-paths": (dict(PLAIN, hostile_paths=True), False),
    "hostile-fileid": (dict(PLAIN, fileid_mismatch=True), False),
    "hostile-dpic": (dict(dpic=0xFFFFFFFF), False),
    "hostile-iter": (dict(iter=1_000_001), False),
    "hostile-keybag-truncated": (dict(keybag_truncate=5), False),
    "hostile-wrap-device-only": (dict(wrap_device_only=True), False),
    "hostile-padding-manifest": (dict(flip_last_byte="manifest"), False),
    "hostile-padding-sms": (dict(flip_last_byte="sms"), False),
    "hostile-size-max": (dict(size_max=True), False),
    "hostile-mbfile-oversize": (dict(PLAIN, mbfile_pad=110_000), False),
    "missing-sms-blob": (dict(PLAIN, omit_sms_blob=True), False),
}


def write_sha256sums(out_dir: str) -> None:
    lines = []
    for dirpath, dirnames, filenames in os.walk(out_dir):
        dirnames.sort()
        for name in sorted(filenames):
            full = os.path.join(dirpath, name)
            rel = os.path.relpath(full, out_dir).replace(os.sep, "/")
            if rel in ("SHA256SUMS", "README.md"):
                continue
            with open(full, "rb") as f:
                digest = hashlib.sha256(f.read()).hexdigest()
            lines.append(f"{digest}  {rel}")
    lines.sort(key=lambda l: l.split("  ", 1)[1])
    with open(os.path.join(out_dir, "SHA256SUMS"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")


def build_catalogue(out_dir: str, seed: int) -> None:
    os.makedirs(out_dir, exist_ok=True)
    for name, (overrides, expected) in CATALOGUE.items():
        o = argparse.Namespace(**dict(defaults(), **overrides))
        # str seeds are hashed deterministically (not affected by PYTHONHASHSEED).
        rng = random.Random(f"{seed}:{name}")
        exp = os.path.join(out_dir, name + ".expected") if expected else None
        if exp and os.path.exists(exp):
            shutil.rmtree(exp)
        build_backup(o, os.path.join(out_dir, name), rng, exp)
        print(f"wrote {name}")
    write_sha256sums(out_dir)
    print(f"python {sys.version.split()[0]}, sqlite {sqlite3.sqlite_version}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--single", metavar="NAME", help="write one backup from the flags below")
    ap.add_argument("--unencrypted", action="store_true")
    ap.add_argument("--password", default=DEFAULT_PASSWORD)
    ap.add_argument("--dpic", type=int, default=DEFAULT_DPIC)
    ap.add_argument("--iter", type=int, default=DEFAULT_ITER)
    ap.add_argument("--snapshot-state", default="finished")
    ap.add_argument("--no-status-plist", action="store_true")
    ap.add_argument("--snapshot-dir", action="store_true")
    ap.add_argument("--omit-sms-blob", action="store_true")
    ap.add_argument("--xml-plists", action="store_true", help="write Manifest.plist and Status.plist as XML")
    ap.add_argument("--xml-mbfile", action="store_true", help="write MBFile records as XML (CF$UID form)")
    ap.add_argument("--legacy-mbdb", action="store_true")
    ap.add_argument("--keybag-truncate", type=int, default=0, metavar="N", help="drop N bytes from the keybag's end")
    ap.add_argument("--wrap-device-only", action="store_true")
    ap.add_argument("--no-manifest-key", action="store_true")
    ap.add_argument("--no-keybag", action="store_true")
    ap.add_argument("--isencrypted-type", choices=["bool", "string"], default="bool")
    ap.add_argument("--flip-last-byte", choices=["manifest", "sms"])
    ap.add_argument("--hostile-paths", action="store_true")
    ap.add_argument("--fileid-mismatch", action="store_true")
    ap.add_argument("--mbfile-pad", type=int, default=0, metavar="BYTES")
    ap.add_argument("--plaintext-manifest-with-encrypted-flag", action="store_true")
    ap.add_argument("--keybag-type", type=int, default=1, metavar="N")
    ap.add_argument("--clas-unused", type=int, metavar="N")
    ap.add_argument("--wpky-unused-len", type=int, metavar="N")
    ap.add_argument("--dup-clas", action="store_true")
    ap.add_argument("--plist-bomb", choices=["info", "status", "manifest", "mbfile"])
    ap.add_argument("--size-max", action="store_true")
    ap.add_argument("--wal-manifest", action="store_true")
    ap.add_argument("--enc-key-shape", choices=["nsdata", "uid-data", "inline"], default="nsdata")
    a = ap.parse_args()
    if a.single:
        rng = random.Random(f"{a.seed}:{a.single}")
        build_backup(a, os.path.join(a.out, a.single), rng, None)
        print(f"wrote {a.single}")
        return 0
    build_catalogue(a.out, a.seed)
    return 0


if __name__ == "__main__":
    sys.exit(main())
