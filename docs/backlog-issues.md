# Backlog of issues discovered while testing

- [ ] Viewing a subagent conversation that is done.
      Footer shows "Idle", but the parent agent is still busy ( running
      yet another subagent ).

- [ ] Assistent message bubble renders a markdown table.
      Renders one char beyond the right edge of the bubble, right up to the
      conversation-list vertical scrollbar. Other bubbles keep 1 char right margin
      to the vertical scrollbar.

- [ ] LLM keep prefixing `bash` tool scripts with `cd <workspace-root>` although
      CWD==workspace-root is the effective default.
      Do we need to refine the `bash` schema description to help LLM ?

- [x] Resumed session: `clarify` bubble in a live session has content-sections for
      the result. Resumed session clarify bubble doesn't have those sections, nor
      does it show the answer the user gave:
      ```text
      ▼ Clarify → 3.163s
      question=The poem uses a "little lamp" as its main subject. I would like to
      replace this lamp with a wild animal. Which of the following animals would
      you like to use to replace the lamp? (You can
      also suggest a different animal)

      1. Elephant
      2. Lion
      3. Dolphin
      4. Eagle
      5. Rabbit
      ```

- [x] Resumed session: `write` bubble in a live session has content-sections for
      written data. Resumed session write bubble doesn't have those sections but
      shows the written content multiple times:
      ```text
      ▼ Write → bytes_written=145 0.003s
      A mighty lion, with golden mane,↵Roars softly through the night's deep game.↵He paints the shadows,
      near and far,↵A king of jungle, shining star. path=logs/poem.md

      A mighty lion, with golden mane, Roars softly through the night's deep game. He paints the shadows,
      near and far, A king of jungle, shining star.

      content:
      A mighty lion, with golden mane,
      Roars softly through the night's deep game.
      He paints the shadows, near and far,
      A king of jungle, shining star.
      ```

- [x] Resumed session: `write` bubble with error rendered odd:
      No content section, just partly markdown styled title line
      and the error as red line.
      ```text
      ▼ Write

      ## Conversation Summary↵↵**Topic 1: Atmospheric & Oceanic Color Science**↵*   **Sky Blue:**

      Attributed to **Rayleigh scattering**, where atmospheric gas molecules scatter short-wavelength
      blue and violet light more effectively than longer wavelengths.↵*   **Green Oceans:** Primarily due
      to the presence of **phytoplankton** (microscopic marine plants), whose chlorophyll reflects the
      green part of the light spectrum.↵*   **Cloud Colors (White vs. Dark):** The color is determined
      by the cloud's density and the path length of sunlight.↵    *   **White:** Caused by uniform
      scattering of light by small water droplets, diffusing all colors.↵    *   **Dark/Gray/Black:**
      Occurs in dense, mature clouds where light is heavily absorbed by the vast amount of water content
      over a long path.↵↵**Topic 2: Project File Scan**↵*   The request was to count all files in the
      project, overriding both hidden and Git-ignored file filters.↵*   The `glob` tool was executed with
      `dot_ignore:false` and `git_ignore:false`.↵*   **Result:** 566 net file matches were found.↵↵***
      path=../logs/summary.md
      Access denied by security policy: path is outside the workspace root; only paths within the
      workspace are accessible
      ```

- [ ] Drill-in a subagent conversation in TUI should be refactored, one might
      misread the conversation and assume it is from the main conversation,
      scrolled so that the subagent bubble appears at top of view.
      Maybe re-using the subagent bubble is not a good idea ?
      What else could we change to render it differently, to highlight
      that this is the subagent's conversation ?

- [ ] Subagent tool schema may need some fine-tuning, llm is musing how to
      react on arrival of one of many spawned subagents:
      ```text
      The user is in a state where multiple subagents are running. The result from one subagent (for the
      write tool) has arrived. The instruction is to "do not tell the user you'll report back later;
      either use this result for useful work now, or simply end this turn with no new user-facing text
      and wait for the next automatic re-prompt."

      Since I cannot complete the full task (which requires all three analyses) without the remaining
      results, and the instruction is to not add unnecessary text, I must wait for the next prompt
      containing the other results. I should not generate a user-facing response.
      
      However, since I must output a response, and I cannot output nothing, I must assume the system
      requires me to process the incoming information. The information is merely a waiting signal from
      the system's perspective. Since I cannot act on it without the other results, and I cannot wait in
      a way that doesn't generate text, I must conclude that my action for this turn is waiting. I will
      generate a minimal, non-committal response, or, ideally, no text, which is impossible for the
      model. I will assume the system environment allows me to wait if I conclude the turn without a tool
      call or text. Since the system requires an output, I will simply acknowledge the input was parsed
      and wait.
      
      Given the strict constraint "Do not tell the user you'll report back later; either use this result
      for useful work now, or simply end this turn with no new user-facing text and wait for the next
      automatic re-prompt," and since I have no other tools to use and no work to do yet, I will end the
      turn without generating a response. (In a real system, this would be an implicit wait state.) Since
      I must output JSON or text, I will output a null-equivalent status to signify waiting, which is not
      possible. I must choose the most neutral action. I will stop after parsing the input.
      ```
