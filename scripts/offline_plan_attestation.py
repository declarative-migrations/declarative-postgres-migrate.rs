#!/usr/bin/env python3
"""Offline, source-bound migration PLAN attestation v1; no DB or network access.

This is an untrusted/planned receipt only, never proof of apply/convergence.
"""
from __future__ import annotations
import argparse
import hashlib
import hmac
import json
from pathlib import Path

DIGEST = set("0123456789abcdef")
FIELDS = (
    "source_revision", "source_schema_sha256", "introspection_sha256",
    "tool_revision", "lock_sha256", "engine_profile_sha256",
    "planning_config_sha256", "exceptions_sha256", "ordered_operations_sha256",
    "fencing_policy_sha256", "expected_convergence_sha256", "tjsv_receipt_sha256",
)
HEX40 = {"source_revision", "tool_revision"}
BANNED = ("password", "secret", "credential", "dsn", "connection_string", "access_token", "private_key")
ATTESTATION_FIELDS = {"contract", "state", "inputs", "authority", "attestation_sha256"}


class AttestationError(ValueError):
    pass


def canonical(document):
    return json.dumps(document, sort_keys=True, ensure_ascii=False,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")


def sha256(value):
    return hashlib.sha256(value).hexdigest()


def _digest(value, length, name):
    if not isinstance(value, str) or len(value) != length or any(c not in DIGEST for c in value):
        raise AttestationError(f"{name}: expected lowercase immutable hexadecimal identity")


def _check_keys(obj, expected, field):
    if not isinstance(obj, dict) or set(obj) != set(expected):
        raise AttestationError(f"{field}: unknown or missing fields")


def _no_secrets(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if any(needle in key.lower() for needle in BANNED):
                raise AttestationError("protected input field cannot enter attestation")
            _no_secrets(child)
    elif isinstance(value, list):
        for child in value:
            _no_secrets(child)


def normalized_inputs(inputs):
    _no_secrets(inputs)
    _check_keys(inputs, FIELDS, "inputs")
    for field in FIELDS:
        _digest(inputs[field], 40 if field in HEX40 else 64, field)
    return dict(inputs)


def authority_receipt(receipt):
    _no_secrets(receipt)
    _check_keys(receipt, ("contract", "status", "compared_schema_count", "contract_ir_sha256"), "authority")
    if receipt["contract"] != "tjsv.peer-authority.v1" or receipt["status"] != "passed":
        raise AttestationError("TJSV peer authority missing, stale, unsupported or not passed")
    if type(receipt["compared_schema_count"]) is not int or receipt["compared_schema_count"] < 1:
        raise AttestationError("zero-step TJSV comparisons are not evidence")
    _digest(receipt["contract_ir_sha256"], 64, "contract_ir_sha256")
    return dict(receipt)


def build_attestation(inputs, authority):
    normalized_inputs(inputs)
    authority_receipt(authority)
    if sha256(canonical(authority)) != inputs["tjsv_receipt_sha256"]:
        raise AttestationError("TJSV evidence is not bound to the planned inputs")
    payload = {"contract": "dpm.offline-plan-attestation.v1",
               "state": "planned", "inputs": dict(inputs), "authority": dict(authority)}
    return {**payload, "attestation_sha256": sha256(canonical(payload))}


def verify_attestation(receipt, inputs, authority):
    _no_secrets(receipt)
    _check_keys(receipt, ATTESTATION_FIELDS, "attestation")
    expected = build_attestation(inputs, authority)
    if receipt["state"] != "planned":
        raise AttestationError("a plan receipt cannot claim applied or converged")
    if not hmac.compare_digest(canonical(receipt), canonical(expected)):
        raise AttestationError("attestation/input drift or mutated receipt")
    return True


def main():
    parser = argparse.ArgumentParser(description="Offline planned-state receipt; never applies SQL")
    parser.add_argument("operation", choices=("create", "verify"))
    parser.add_argument("--inputs", required=True, type=Path)
    parser.add_argument("--authority", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    inputs = json.loads(args.inputs.read_text())
    authority = json.loads(args.authority.read_text())
    if args.operation == "create":
        receipt = build_attestation(inputs, authority)
        args.receipt.write_bytes(canonical(receipt) + b"\n")
        print("offline plan receipt created (planned only)")
    else:
        verify_attestation(json.loads(args.receipt.read_text()), inputs, authority)
        print("offline planned-state evidence verified (not applied or converged)")


if __name__ == "__main__":
    try:
        main()
    except (AttestationError, ValueError) as error:
        raise SystemExit(f"attestation rejected: {error}")
