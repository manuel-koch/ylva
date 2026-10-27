# Backlog of issues discovered while testing

- [x] Restore the terminal (mouse capture, bracketed paste) on panic and on
      SIGTERM/SIGHUP; today only a clean exit restores it, so an abnormal exit
      leaves the terminal emitting garbage on mouse events.

- [x] Add a hard upper bound (e.g. 50 MB) on pasted content, rejecting larger
      pastes with a toast, to protect against memory blow-ups.

- [ ] Wide terminal: Bash approval dialog is rendered like:
      ```text
      Tool 'bash' requests your approval:    <provider/model/reasoning>
      [ Approve ]   [ Deny ]                 <costs> <context-usage>
      <input field for denial>
      ```
      But for a narrow terminal it is oddly rendered like:
      ```text
      Tool 'bash' requests your approval:    
      <provider/model/reasoning> <costs> <context-usage>
      [ Approve ]   [ Deny ]                 
      <input field for denial>
      ```
      We should render approval for narrow terminal like the following instead:
      ```text
      <provider/model/reasoning> <costs> <context-usage>
      Tool 'bash' requests your approval:    
      [ Approve ]   [ Deny ]                 
      <input field for denial>
      ```
