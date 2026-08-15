-- When a repeating pattern was detected, offered, and the user said no.
--
-- ## The gap this closes
--
-- Until now, "no pattern was ever found" and "a pattern was found and you
-- declined it" produced identical rows: `template_state = 'none'`, nothing
-- else. `no_template_reason` explains the first case, but it lives in the
-- capture summary and is never persisted -- and it is only ever set when
-- detection found nothing, so it is silent on the second case by construction.
--
-- A user asking later why a playbook is not repeating therefore got nothing,
-- in the one case where there IS an answer: they were asked, and they said no.
--
-- ## Why a timestamp column rather than a fourth `template_state`
--
-- 'declined' as a lifecycle value would be the tidier model, and it was the
-- first design. It was rejected on a practical ground: SQLite cannot alter a
-- CHECK constraint, so adding a value to `template_state IN (...)` means
-- rebuilding `playbooks` against live user data -- for a fact that is not
-- actually part of that lifecycle.
--
-- It is not part of it because the two answer different questions.
-- `template_state` answers "is this a repeating workflow", and a declined
-- recording is emphatically not one -- it is an ordinary playbook, and §4.10 is
-- explicit that a declined proposal leaves "an ordinary one-shot playbook,
-- unaffected". This column answers a different question: "was one offered".
-- Keeping them separate means a declined playbook behaves in every existing
-- code path exactly as it did before, because to those paths it IS 'none'.
--
-- The timestamp is free information over a boolean, in the same format as every
-- other time in this schema, and answers "when were they asked" without a
-- second column.
ALTER TABLE playbooks ADD COLUMN template_declined_at TEXT
    CHECK (
        template_declined_at IS NULL
     OR template_state <> 'confirmed'
    );

-- Confirmed and declined are mutually exclusive, and the CHECK above makes that
-- structural rather than a rule every writer has to remember -- the same
-- reasoning as the `template_confirmed_at` pairing it sits beside.
--
-- Worth stating what is deliberately NOT constrained: a row may have
-- `template_state = 'none'` with the timestamp either set or NULL, because that
-- is precisely the distinction being added. Requiring one to imply the other
-- would collapse the two cases back together.
