# Approval Verdict Trail

Every `ApprovalCheck` should carry info to be able to trace where / how it
originated. That could be helpful in logs to trace approvals.

- `decision_maker`: Source module / class
- `rule_origin`: From where did the rule come, that triggered the verdict
  ( cli-option, config, default )
- `parent`: If current decision was based on another approval
