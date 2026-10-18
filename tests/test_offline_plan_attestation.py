import copy
import hashlib
import json
import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from offline_plan_attestation import (
    AttestationError, FIELDS, HEX40, build_attestation,
    canonical, sha256, verify_attestation,
)


class OfflinePlanAttestationTests(unittest.TestCase):
    def setUp(self):
        self.authority = {
            "contract": "tjsv.peer-authority.v1", "status": "passed",
            "compared_schema_count": 2, "contract_ir_sha256": "a" * 64,
        }
        self.inputs = {field: ("b" * (40 if field in HEX40 else 64)) for field in FIELDS}
        self.inputs["tjsv_receipt_sha256"] = sha256(canonical(self.authority))

    def test_reproducible_and_order_independent_authority_object(self):
        a = build_attestation(self.inputs, self.authority)
        b = build_attestation(dict(reversed(list(self.inputs.items()))),
                              dict(reversed(list(self.authority.items()))))
        self.assertEqual(canonical(a), canonical(b))
        self.assertTrue(verify_attestation(a, self.inputs, self.authority))
        self.assertEqual(a["state"], "planned")

    def test_tamper_each_bound_component(self):
        original = build_attestation(self.inputs, self.authority)
        for field in FIELDS:
            changed = dict(self.inputs)
            changed[field] = ("c" * (40 if field in HEX40 else 64))
            with self.subTest(field=field), self.assertRaises(AttestationError):
                verify_attestation(original, changed, self.authority)

    def test_statement_order_digest_is_part_of_identity(self):
        operations = ["CREATE TABLE a", "ALTER TABLE a ADD COLUMN b"]
        forward = sha256(canonical(operations))
        reverse = sha256(canonical(list(reversed(operations))))
        self.assertNotEqual(forward, reverse)
        self.inputs["ordered_operations_sha256"] = forward
        receipt = build_attestation(self.inputs, self.authority)
        self.inputs["ordered_operations_sha256"] = reverse
        with self.assertRaises(AttestationError):
            verify_attestation(receipt, self.inputs, self.authority)

    def test_fail_closed_on_stale_zero_step_or_nonpassed_tjsv(self):
        for status, count in [("blocked", 2), ("passed", 0), ("not_run", 2)]:
            bad = dict(self.authority, status=status, compared_schema_count=count)
            bound = dict(self.inputs, tjsv_receipt_sha256=sha256(canonical(bad)))
            with self.subTest(status=status, count=count), self.assertRaises(AttestationError):
                build_attestation(bound, bad)

    def test_plan_cannot_masquerade_as_apply_or_convergence(self):
        receipt = build_attestation(self.inputs, self.authority)
        for state in ("applied", "converged", "failed", "blocked", "not_run"):
            changed = dict(receipt, state=state)
            with self.subTest(state=state), self.assertRaises(AttestationError):
                verify_attestation(changed, self.inputs, self.authority)

    def test_secret_inputs_and_unreviewed_extra_fields_rejected(self):
        for extra in ("database_dsn", "access_token", "password", "unreviewed"):
            with self.subTest(field=extra), self.assertRaises(AttestationError):
                build_attestation({**self.inputs, extra: "do-not-record"}, self.authority)


if __name__ == "__main__":
    unittest.main()
