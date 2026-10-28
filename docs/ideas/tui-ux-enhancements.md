# TUI UX enhancements

Going out of a drilled-in subagent conversation using `esc` key can
accidentially abort a tool approval or clarify that pops up just
instantly before the user can actually recognize it.
Maybe another keybinding would be better ?
Maybe `ctrl-p` or `F2` or a dedicated `back to parent` button in the
subagent header bubble ?

Similar misbehavior can occure while writing a draft message
and hitting enter ( while user beliefs he is still editing the draft message
and using the enter to send it ), a tool approval arrived and
parts of the last written text end up in the denial-draft-message
of approval-dialog and the final enter is a approval-denied
instead of a sent-draft-message.
Should approve/deny be a dedicated keybinding instead of
`enter` or `esc`, which can have other meaning in the normal
UX in TUI ?

Or should tool approval only be triggered by explicit click on the
`approve` or `deny` button and not via `enter`or `esc` keybinding ?
