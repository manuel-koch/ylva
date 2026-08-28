# Approval verdict trail

Every `ApprovalCheck` should carry enough info to trace where and how it
originated. Useful in logs to trace approvals.

- `decision_maker`: source module / class
- `rule_origin`: where the triggering rule came from (cli-option, config, default)
- `parent`: if the current decision was based on another approval
