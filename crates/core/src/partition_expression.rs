//! partition_expression : VersionedPartitionShape × ScalarRuleExpr × TypedReferences ⇀ PartitionExpr   (pure, deterministic)

use crate::canonical_encoding::{
    CanonicalEncode, CanonicalEncodingError, CanonicalField, CanonicalPayloadWriter,
};
use crate::identity::SubstanceId;
use crate::non_negative_amount::NonNegativeAmount;
use crate::numerical_semantics::{AccumulationError, NumericalSemanticsVersion};
use crate::rule_expression::RuleExpr;
use crate::rule_reference::{ExpressionValueKind, ForcingRef, TransferBranchId};
use crate::versions::RuleIrVersion;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fraction {
    semantics: NumericalSemanticsVersion,
    bits: u64,
}
impl Fraction {
    /// Creates a canonical finite fraction in the inclusive interval from zero to one.
    ///
    /// # Errors
    ///
    /// Returns [`FractionError::OutOfRange`] when `value` is non-finite or outside the interval.
    pub fn new(semantics: NumericalSemanticsVersion, value: f64) -> Result<Self, FractionError> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(FractionError::OutOfRange {
                bits: value.to_bits(),
            });
        }
        Ok(Self {
            semantics,
            bits: if value == 0.0 { 0 } else { value.to_bits() },
        })
    }
    pub fn numerical_semantics_version(self) -> NumericalSemanticsVersion {
        self.semantics
    }
    pub fn value(self) -> f64 {
        f64::from_bits(self.bits)
    }
    pub fn bits(self) -> u64 {
        self.bits
    }
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub enum FractionError {
    /// Fires when a fraction is non-finite or outside the inclusive interval from zero to one.
    #[error("fraction is outside the inclusive finite interval: bits {bits:#018x}")]
    OutOfRange { bits: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FractionBranch {
    branch: TransferBranchId,
    fraction: Fraction,
}
impl FractionBranch {
    pub fn new(branch: TransferBranchId, fraction: Fraction) -> Self {
        Self { branch, fraction }
    }
    pub fn branch(&self) -> &TransferBranchId {
        &self.branch
    }
    pub fn fraction(&self) -> Fraction {
        self.fraction
    }
}

/// One named transfer whose amount is computed by a scalar rule expression.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExpressionBranch {
    branch: TransferBranchId,
    expression: RuleExpr,
}
impl ExpressionBranch {
    #[must_use]
    pub fn new(branch: TransferBranchId, expression: RuleExpr) -> Self {
        Self { branch, expression }
    }
    #[must_use]
    pub fn branch(&self) -> &TransferBranchId {
        &self.branch
    }
    #[must_use]
    pub fn expression(&self) -> &RuleExpr {
        &self.expression
    }
}

/// One dependent transfer mapped to a realised carrier branch at the same compartment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CarrierBranch {
    branch: TransferBranchId,
    carrier_branch: TransferBranchId,
}
impl CarrierBranch {
    #[must_use]
    pub fn new(branch: TransferBranchId, carrier_branch: TransferBranchId) -> Self {
        Self {
            branch,
            carrier_branch,
        }
    }
    #[must_use]
    pub fn branch(&self) -> &TransferBranchId {
        &self.branch
    }
    #[must_use]
    pub fn carrier_branch(&self) -> &TransferBranchId {
        &self.carrier_branch
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PartitionExprError {
    /// Fires when a partition names one transfer branch more than once.
    #[error("partition contains duplicate branch {branch}")]
    DuplicateBranch { branch: TransferBranchId },
    /// Fires when a dependent partition maps one carrier branch more than once.
    #[error("partition maps carrier branch {branch} more than once")]
    DuplicateCarrierBranch { branch: TransferBranchId },
    /// Fires when an expression-valued branch is not scalar.
    #[error("expression branch {branch} must be scalar")]
    NonScalarExpression { branch: TransferBranchId },
    /// Fires when an expression-valued branch selects a different rule IR version.
    #[error("expression branch {branch} selects a different rule IR version")]
    ExpressionRuleIrVersionMismatch { branch: TransferBranchId },
    /// Fires when an expression-valued branch selects different numerical semantics.
    #[error("expression branch {branch} selects different numerical semantics")]
    ExpressionNumericalVersionMismatch { branch: TransferBranchId },
    /// Fires when checked fraction accumulation cannot produce a finite total.
    #[error("fixed split fraction accumulation failed: {cause}")]
    FractionAccumulation {
        #[source]
        cause: AccumulationError,
    },
    /// Fires when a fixed split's exact ordered fraction total is not one.
    #[error("fixed split fractions have invalid total bits {total_bits:#018x}")]
    InvalidFixedFractionTotal { total_bits: u64 },
}

fn fraction_accumulation_error(cause: AccumulationError) -> PartitionExprError {
    PartitionExprError::FractionAccumulation { cause }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PartitionNode {
    RetainAll,
    ReleaseAll {
        branch: TransferBranchId,
    },
    FixedFractionSplit {
        retained_fraction: Fraction,
        branches: Vec<FractionBranch>,
    },
    ExogenousSeries {
        branch: TransferBranchId,
        series: ForcingRef,
    },
    ConstantFractionTransfer {
        branch: TransferBranchId,
        fraction: Fraction,
    },
    ExpressionPartition {
        branches: Vec<ExpressionBranch>,
    },
    CarrierProportional {
        carrier: SubstanceId,
        branches: Vec<CarrierBranch>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartitionExpr {
    rule_ir: RuleIrVersion,
    semantics: NumericalSemanticsVersion,
    // Fraction carriers must retain the same semantics version. Adding a version requires revisiting this invariant.
    node: PartitionNode,
}

pub enum PartitionExprView<'a> {
    RetainAll,
    ReleaseAll {
        branch: &'a TransferBranchId,
    },
    FixedFractionSplit {
        retained_fraction: Fraction,
        branches: &'a [FractionBranch],
    },
    ExogenousSeries {
        branch: &'a TransferBranchId,
        series: &'a ForcingRef,
    },
    ConstantFractionTransfer {
        branch: &'a TransferBranchId,
        fraction: Fraction,
    },
    ExpressionPartition {
        branches: &'a [ExpressionBranch],
    },
    CarrierProportional {
        carrier: &'a SubstanceId,
        branches: &'a [CarrierBranch],
    },
}

impl PartitionExpr {
    pub fn retain_all(rule_ir: RuleIrVersion, semantics: NumericalSemanticsVersion) -> Self {
        Self {
            rule_ir,
            semantics,
            node: PartitionNode::RetainAll,
        }
    }
    pub fn release_all(
        rule_ir: RuleIrVersion,
        semantics: NumericalSemanticsVersion,
        branch: TransferBranchId,
    ) -> Self {
        Self {
            rule_ir,
            semantics,
            node: PartitionNode::ReleaseAll { branch },
        }
    }
    /// Creates an exhaustive fixed-fraction split in canonical branch order.
    ///
    /// # Errors
    ///
    /// Returns [`PartitionExprError`] for duplicate branches, accumulation failure, or a total other than one.
    pub fn fixed_fraction_split(
        rule_ir: RuleIrVersion,
        semantics: NumericalSemanticsVersion,
        retained_fraction: Fraction,
        mut branches: Vec<FractionBranch>,
    ) -> Result<Self, PartitionExprError> {
        branches.sort_by(|left, right| left.branch.cmp(&right.branch));
        if let Some(pair) = branches
            .windows(2)
            .find(|pair| pair[0].branch == pair[1].branch)
        {
            return Err(PartitionExprError::DuplicateBranch {
                branch: pair[0].branch.clone(),
            });
        }
        let retained = NonNegativeAmount::try_from(retained_fraction.value()).map_err(|_| {
            PartitionExprError::InvalidFixedFractionTotal {
                total_bits: retained_fraction.bits(),
            }
        })?;
        let transfer = branches
            .iter()
            .map(|branch| NonNegativeAmount::try_from(branch.fraction.value()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| PartitionExprError::InvalidFixedFractionTotal {
                total_bits: retained_fraction.bits(),
            })?;
        let total = semantics
            .accumulate(std::iter::once(retained).chain(transfer))
            .map_err(fraction_accumulation_error)?;
        if total.value().to_bits() != 1.0_f64.to_bits() {
            return Err(PartitionExprError::InvalidFixedFractionTotal {
                total_bits: total.value().to_bits(),
            });
        }
        Ok(Self {
            rule_ir,
            semantics,
            node: PartitionNode::FixedFractionSplit {
                retained_fraction,
                branches,
            },
        })
    }
    pub fn exogenous_series(
        rule_ir: RuleIrVersion,
        semantics: NumericalSemanticsVersion,
        branch: TransferBranchId,
        series: ForcingRef,
    ) -> Self {
        Self {
            rule_ir,
            semantics,
            node: PartitionNode::ExogenousSeries { branch, series },
        }
    }
    /// Creates a constant-fraction transfer shape.
    ///
    /// # Errors
    ///
    /// With the currently closed version enums, this constructor does not return an error.
    pub fn constant_fraction_transfer(
        rule_ir: RuleIrVersion,
        semantics: NumericalSemanticsVersion,
        branch: TransferBranchId,
        fraction: Fraction,
    ) -> Result<Self, PartitionExprError> {
        Ok(Self {
            rule_ir,
            semantics,
            node: PartitionNode::ConstantFractionTransfer { branch, fraction },
        })
    }
    /// Creates a partition whose named transfers are independently computed expressions.
    ///
    /// Branches are stored in canonical identity order. Their evaluated sum is checked against
    /// available stock during execution; all stock left by that sum is retained.
    ///
    /// # Errors
    ///
    /// Returns [`PartitionExprError`] for duplicate branches, non-scalar expressions, or
    /// expressions selecting protocol versions different from the partition.
    pub fn expression_partition(
        rule_ir: RuleIrVersion,
        semantics: NumericalSemanticsVersion,
        mut branches: Vec<ExpressionBranch>,
    ) -> Result<Self, PartitionExprError> {
        branches.sort_by(|left, right| left.branch.cmp(&right.branch));
        if let Some(pair) = branches
            .windows(2)
            .find(|pair| pair[0].branch == pair[1].branch)
        {
            return Err(PartitionExprError::DuplicateBranch {
                branch: pair[0].branch.clone(),
            });
        }
        for branch in &branches {
            if branch.expression.value_kind() != ExpressionValueKind::Scalar {
                return Err(PartitionExprError::NonScalarExpression {
                    branch: branch.branch.clone(),
                });
            }
            if branch.expression.rule_ir_version() != rule_ir {
                return Err(PartitionExprError::ExpressionRuleIrVersionMismatch {
                    branch: branch.branch.clone(),
                });
            }
            if branch.expression.numerical_semantics_version() != semantics {
                return Err(PartitionExprError::ExpressionNumericalVersionMismatch {
                    branch: branch.branch.clone(),
                });
            }
        }
        Ok(Self {
            rule_ir,
            semantics,
            node: PartitionNode::ExpressionPartition { branches },
        })
    }
    /// Allocates dependent counts in proportion to realised carrier counts.
    ///
    /// Each mapped branch receives floor(dependent_available * carrier_branch / carrier_available).
    /// Zero carrier stock retains every dependent count. Unmapped counts remain at the source.
    /// Cross-rule carrier and destination checks occur when the model artifact is built.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate dependent or carrier branch identities.
    pub fn carrier_proportional(
        rule_ir: RuleIrVersion,
        semantics: NumericalSemanticsVersion,
        carrier: SubstanceId,
        mut branches: Vec<CarrierBranch>,
    ) -> Result<Self, PartitionExprError> {
        branches.sort_by(|left, right| left.branch.cmp(&right.branch));
        let mut carriers = std::collections::BTreeSet::new();
        for (index, branch) in branches.iter().enumerate() {
            if index > 0 && branches[index - 1].branch == branch.branch {
                return Err(PartitionExprError::DuplicateBranch {
                    branch: branch.branch.clone(),
                });
            }
            if !carriers.insert(&branch.carrier_branch) {
                return Err(PartitionExprError::DuplicateCarrierBranch {
                    branch: branch.carrier_branch.clone(),
                });
            }
        }
        Ok(Self {
            rule_ir,
            semantics,
            node: PartitionNode::CarrierProportional { carrier, branches },
        })
    }
    pub fn rule_ir_version(&self) -> RuleIrVersion {
        self.rule_ir
    }
    pub fn numerical_semantics_version(&self) -> NumericalSemanticsVersion {
        self.semantics
    }
    pub fn view(&self) -> PartitionExprView<'_> {
        match &self.node {
            PartitionNode::RetainAll => PartitionExprView::RetainAll,
            PartitionNode::ReleaseAll { branch } => PartitionExprView::ReleaseAll { branch },
            PartitionNode::FixedFractionSplit {
                retained_fraction,
                branches,
            } => PartitionExprView::FixedFractionSplit {
                retained_fraction: *retained_fraction,
                branches,
            },
            PartitionNode::ExogenousSeries { branch, series } => {
                PartitionExprView::ExogenousSeries { branch, series }
            }
            PartitionNode::ConstantFractionTransfer { branch, fraction } => {
                PartitionExprView::ConstantFractionTransfer {
                    branch,
                    fraction: *fraction,
                }
            }
            PartitionNode::ExpressionPartition { branches } => {
                PartitionExprView::ExpressionPartition { branches }
            }
            PartitionNode::CarrierProportional { carrier, branches } => {
                PartitionExprView::CarrierProportional { carrier, branches }
            }
        }
    }
}

#[derive(Serialize)]
struct PartitionWireRef<'a> {
    rule_ir_version: RuleIrVersion,
    numerical_semantics_version: NumericalSemanticsVersion,
    partition: PartitionNodeRef<'a>,
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PartitionNodeRef<'a> {
    RetainAll,
    ReleaseAll {
        branch: &'a TransferBranchId,
    },
    FixedFractionSplit {
        retained_fraction: f64,
        branches: Vec<FractionBranchRef<'a>>,
    },
    ExogenousSeries {
        branch: &'a TransferBranchId,
        series: &'a ForcingRef,
    },
    ConstantFractionTransfer {
        branch: &'a TransferBranchId,
        fraction: f64,
    },
    ExpressionPartition {
        branches: &'a [ExpressionBranch],
    },
    CarrierProportional {
        carrier: &'a str,
        branches: &'a [CarrierBranch],
    },
}
#[derive(Serialize)]
struct FractionBranchRef<'a> {
    branch: &'a TransferBranchId,
    fraction: f64,
}
impl Serialize for PartitionExpr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let partition = match &self.node {
            PartitionNode::RetainAll => PartitionNodeRef::RetainAll,
            PartitionNode::ReleaseAll { branch } => PartitionNodeRef::ReleaseAll { branch },
            PartitionNode::FixedFractionSplit {
                retained_fraction,
                branches,
            } => PartitionNodeRef::FixedFractionSplit {
                retained_fraction: retained_fraction.value(),
                branches: branches
                    .iter()
                    .map(|b| FractionBranchRef {
                        branch: &b.branch,
                        fraction: b.fraction.value(),
                    })
                    .collect(),
            },
            PartitionNode::ExogenousSeries { branch, series } => {
                PartitionNodeRef::ExogenousSeries { branch, series }
            }
            PartitionNode::ConstantFractionTransfer { branch, fraction } => {
                PartitionNodeRef::ConstantFractionTransfer {
                    branch,
                    fraction: fraction.value(),
                }
            }
            PartitionNode::ExpressionPartition { branches } => {
                PartitionNodeRef::ExpressionPartition { branches }
            }
            PartitionNode::CarrierProportional { carrier, branches } => {
                PartitionNodeRef::CarrierProportional {
                    carrier: carrier.as_str(),
                    branches,
                }
            }
        };
        PartitionWireRef {
            rule_ir_version: self.rule_ir,
            numerical_semantics_version: self.semantics,
            partition,
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionWire {
    rule_ir_version: RuleIrVersion,
    numerical_semantics_version: NumericalSemanticsVersion,
    partition: OwnedNode,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum OwnedNode {
    RetainAll,
    ReleaseAll {
        branch: TransferBranchId,
    },
    FixedFractionSplit {
        retained_fraction: f64,
        branches: Vec<OwnedBranch>,
    },
    ExogenousSeries {
        branch: TransferBranchId,
        series: ForcingRef,
    },
    ConstantFractionTransfer {
        branch: TransferBranchId,
        fraction: f64,
    },
    ExpressionPartition {
        branches: Vec<ExpressionBranch>,
    },
    CarrierProportional {
        carrier: String,
        branches: Vec<CarrierBranch>,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedBranch {
    branch: TransferBranchId,
    fraction: f64,
}
impl<'de> Deserialize<'de> for PartitionExpr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = PartitionWire::deserialize(deserializer)?;
        let r = wire.rule_ir_version;
        let s = wire.numerical_semantics_version;
        match wire.partition {
            OwnedNode::RetainAll => Ok(Self::retain_all(r, s)),
            OwnedNode::ReleaseAll { branch } => Ok(Self::release_all(r, s, branch)),
            OwnedNode::FixedFractionSplit {
                retained_fraction,
                branches,
            } => {
                let retained =
                    Fraction::new(s, retained_fraction).map_err(serde::de::Error::custom)?;
                let branches = branches
                    .into_iter()
                    .map(|b| Fraction::new(s, b.fraction).map(|f| FractionBranch::new(b.branch, f)))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(serde::de::Error::custom)?;
                Self::fixed_fraction_split(r, s, retained, branches)
                    .map_err(serde::de::Error::custom)
            }
            OwnedNode::ExogenousSeries { branch, series } => {
                Ok(Self::exogenous_series(r, s, branch, series))
            }
            OwnedNode::ConstantFractionTransfer { branch, fraction } => {
                let fraction = Fraction::new(s, fraction).map_err(serde::de::Error::custom)?;
                Self::constant_fraction_transfer(r, s, branch, fraction)
                    .map_err(serde::de::Error::custom)
            }
            OwnedNode::ExpressionPartition { branches } => {
                Self::expression_partition(r, s, branches).map_err(serde::de::Error::custom)
            }
            OwnedNode::CarrierProportional { carrier, branches } => Self::carrier_proportional(
                r,
                s,
                SubstanceId::parse(&carrier).map_err(serde::de::Error::custom)?,
                branches,
            )
            .map_err(serde::de::Error::custom),
        }
    }
}

impl CanonicalEncode for PartitionExpr {
    fn root_tag(&self) -> u16 {
        0x0017
    }
    fn encode_payload(&self, w: &mut CanonicalPayloadWriter) -> Result<(), CanonicalEncodingError> {
        w.write_u16(match self.rule_ir {
            RuleIrVersion::V1 => 1,
        });
        w.write_u16(match self.semantics {
            NumericalSemanticsVersion::V1 => 1,
            NumericalSemanticsVersion::V2 => 2,
        });
        match &self.node {
            PartitionNode::RetainAll => w.write_u8(0x00),
            PartitionNode::ReleaseAll { branch } => {
                w.write_u8(0x01);
                w.write_string(CanonicalField::PartitionBranchIdentity, branch.as_str())?
            }
            PartitionNode::FixedFractionSplit {
                retained_fraction,
                branches,
            } => {
                w.write_u8(0x02);
                w.write_scalar(CanonicalField::PartitionFraction, retained_fraction.value())?;
                w.write_count(CanonicalField::PartitionBranches, branches.len())?;
                for branch in branches {
                    w.write_string(
                        CanonicalField::PartitionBranchIdentity,
                        branch.branch.as_str(),
                    )?;
                    w.write_scalar(CanonicalField::PartitionFraction, branch.fraction.value())?
                }
            }
            PartitionNode::ExogenousSeries { branch, series } => {
                w.write_u8(0x03);
                w.write_string(CanonicalField::PartitionBranchIdentity, branch.as_str())?;
                w.write_string(CanonicalField::RuleForcingIdentity, series.id().as_str())?
            }
            PartitionNode::ConstantFractionTransfer { branch, fraction } => {
                w.write_u8(0x04);
                w.write_string(CanonicalField::PartitionBranchIdentity, branch.as_str())?;
                w.write_scalar(CanonicalField::PartitionFraction, fraction.value())?
            }
            PartitionNode::ExpressionPartition { branches } => {
                w.write_u8(0x05);
                w.write_count(CanonicalField::PartitionBranches, branches.len())?;
                for branch in branches {
                    w.write_string(
                        CanonicalField::PartitionBranchIdentity,
                        branch.branch.as_str(),
                    )?;
                    branch.expression.encode_payload_unframed(w)?;
                }
            }
            PartitionNode::CarrierProportional { carrier, branches } => {
                w.write_u8(0x06);
                w.write_string(CanonicalField::PartitionCarrierSubstance, carrier.as_str())?;
                w.write_count(CanonicalField::PartitionBranches, branches.len())?;
                for branch in branches {
                    w.write_string(
                        CanonicalField::PartitionBranchIdentity,
                        branch.branch.as_str(),
                    )?;
                    w.write_string(
                        CanonicalField::PartitionBranchIdentity,
                        branch.carrier_branch.as_str(),
                    )?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::numerical_semantics::{AccumulationError, NumericalSemanticsVersion};
    use crate::partition_expression::{PartitionExprError, fraction_accumulation_error};
    #[test]
    fn accumulation_error_mapping_preserves_the_complete_cause() {
        let actual = fraction_accumulation_error(AccumulationError::NonFiniteSum {
            version: NumericalSemanticsVersion::V1,
            index: 3,
            partial_sum: f64::MAX,
            addend: 0.875,
        });
        assert_eq!(
            actual,
            PartitionExprError::FractionAccumulation {
                cause: AccumulationError::NonFiniteSum {
                    version: NumericalSemanticsVersion::V1,
                    index: 3,
                    partial_sum: f64::MAX,
                    addend: 0.875
                }
            }
        );
    }
}
