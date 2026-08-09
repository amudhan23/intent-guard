// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title IntentGuardAuditLog
/// @notice An append-only record that a specific IntentGuard decision was made.
///
/// Rain sees only payment data. IntentGuard sees the task. But without this,
/// the fact that a decision was made — and on what grounds — exists only in
/// IntentGuard's own printed output, which no third party can verify after the
/// fact. Anchoring the decision here makes "this exact decision was made, at
/// this time" provable without trusting IntentGuard's logs.
///
/// Only a hash goes on chain. The destination, budget, and purpose stay
/// private; anyone holding the original task can recompute the hash and prove
/// it matches, but the chain itself reveals nothing about where the user went
/// or what they could spend.
contract IntentGuardAuditLog {
    struct DecisionRecord {
        bytes32 decisionHash;
        string status; // "approved" | "blocked" | "escalated"
        uint256 timestamp;
    }

    DecisionRecord[] public records;

    event DecisionLogged(
        uint256 indexed index,
        bytes32 decisionHash,
        string status,
        uint256 timestamp
    );

    function logDecision(bytes32 decisionHash, string calldata status) external {
        records.push(DecisionRecord(decisionHash, status, block.timestamp));
        emit DecisionLogged(records.length - 1, decisionHash, status, block.timestamp);
    }

    function getRecord(uint256 index) external view returns (bytes32, string memory, uint256) {
        DecisionRecord storage r = records[index];
        return (r.decisionHash, r.status, r.timestamp);
    }

    function recordCount() external view returns (uint256) {
        return records.length;
    }
}
