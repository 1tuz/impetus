//! Proposal-only instruction learning.
//!
//! This module keeps observed evidence and proposal state in memory. It does
//! not discover instruction files, construct paths, or mutate the filesystem.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationKind {
    ReusableFact,
    RepeatedRule,
    GuideCorrection,
    ProcedureGap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalTarget {
    Memory,
    Convention,
    GuideUpdate,
    SkillImprovement,
}

impl ProposalTarget {
    const fn promotion_threshold(self) -> usize {
        match self {
            Self::Memory => 1,
            Self::Convention | Self::GuideUpdate => 3,
            Self::SkillImprovement => 5,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProposalLifecycle {
    Observed,
    Candidate,
    Repeated,
    Validated,
    Proposed,
    Promoted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LearningEvidence {
    pub kind: ObservationKind,
    pub summary: String,
}

impl LearningEvidence {
    pub fn new(kind: ObservationKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: summary.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub target: ProposalTarget,
    pub lifecycle: ProposalLifecycle,
    pub evidence: Vec<LearningEvidence>,
}

impl Proposal {
    fn from_evidence(evidence: LearningEvidence) -> Self {
        Self {
            target: classify_target(evidence.kind),
            lifecycle: ProposalLifecycle::Observed,
            evidence: vec![evidence],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstructionLearning {
    proposal: Proposal,
}

impl InstructionLearning {
    pub fn new(evidence: LearningEvidence) -> Self {
        Self {
            proposal: Proposal::from_evidence(evidence),
        }
    }

    pub fn observe(&mut self, evidence: LearningEvidence) {
        self.proposal.evidence.push(evidence);
    }

    pub fn advance(&mut self) {
        self.proposal.lifecycle = match self.proposal.lifecycle {
            ProposalLifecycle::Observed => ProposalLifecycle::Candidate,
            ProposalLifecycle::Candidate => ProposalLifecycle::Repeated,
            ProposalLifecycle::Repeated => ProposalLifecycle::Validated,
            ProposalLifecycle::Validated => ProposalLifecycle::Proposed,
            ProposalLifecycle::Proposed
                if self.proposal.evidence.len() >= self.proposal.target.promotion_threshold() =>
            {
                ProposalLifecycle::Promoted
            }
            lifecycle => lifecycle,
        };
    }

    pub fn proposal(&self) -> &Proposal {
        &self.proposal
    }
}

const fn classify_target(kind: ObservationKind) -> ProposalTarget {
    match kind {
        ObservationKind::ReusableFact => ProposalTarget::Memory,
        ObservationKind::RepeatedRule => ProposalTarget::Convention,
        ObservationKind::GuideCorrection => ProposalTarget::GuideUpdate,
        ObservationKind::ProcedureGap => ProposalTarget::SkillImprovement,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(kind: ObservationKind) -> LearningEvidence {
        LearningEvidence::new(kind, "observed outcome")
    }

    fn advance_to_proposed(learning: &mut InstructionLearning) {
        for _ in 0..4 {
            learning.advance();
        }
    }

    #[test]
    fn classifies_observed_evidence_into_proposal_targets() {
        let cases = [
            (ObservationKind::ReusableFact, ProposalTarget::Memory),
            (ObservationKind::RepeatedRule, ProposalTarget::Convention),
            (
                ObservationKind::GuideCorrection,
                ProposalTarget::GuideUpdate,
            ),
            (
                ObservationKind::ProcedureGap,
                ProposalTarget::SkillImprovement,
            ),
        ];

        for (kind, target) in cases {
            assert_eq!(
                InstructionLearning::new(evidence(kind)).proposal().target,
                target
            );
        }
    }

    #[test]
    fn advances_through_proposal_lifecycle_in_order() {
        let mut learning = InstructionLearning::new(evidence(ObservationKind::RepeatedRule));

        assert_eq!(learning.proposal().lifecycle, ProposalLifecycle::Observed);

        let states = [
            ProposalLifecycle::Candidate,
            ProposalLifecycle::Repeated,
            ProposalLifecycle::Validated,
            ProposalLifecycle::Proposed,
        ];
        for state in states {
            learning.advance();
            assert_eq!(learning.proposal().lifecycle, state);
        }
    }

    #[test]
    fn promotes_convention_after_its_threshold() {
        let mut learning = InstructionLearning::new(evidence(ObservationKind::RepeatedRule));
        learning.observe(evidence(ObservationKind::RepeatedRule));
        learning.observe(evidence(ObservationKind::RepeatedRule));

        advance_to_proposed(&mut learning);
        learning.advance();

        assert_eq!(learning.proposal().lifecycle, ProposalLifecycle::Promoted);
    }

    #[test]
    fn keeps_skill_proposed_below_its_stricter_threshold() {
        let mut learning = InstructionLearning::new(evidence(ObservationKind::ProcedureGap));
        learning.observe(evidence(ObservationKind::ProcedureGap));
        learning.observe(evidence(ObservationKind::ProcedureGap));

        advance_to_proposed(&mut learning);
        learning.advance();

        assert_eq!(learning.proposal().lifecycle, ProposalLifecycle::Proposed);
    }

    #[test]
    fn observing_and_advancing_never_creates_instruction_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let before = std::fs::read_dir(directory.path())
            .expect("read temporary directory")
            .count();
        let mut learning = InstructionLearning::new(evidence(ObservationKind::GuideCorrection));

        advance_to_proposed(&mut learning);

        let after = std::fs::read_dir(directory.path())
            .expect("read temporary directory")
            .count();
        assert_eq!(after, before);
    }
}
