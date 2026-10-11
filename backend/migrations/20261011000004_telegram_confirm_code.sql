-- Ferry schema, part 10: Telegram sign-in's matching confirm code and its one-shot token (contract v1.3.1;
-- docs/CONTRACT_REQUESTS.md, "Telegram sign-in: confirm in the bot before verifying"; docs/DECISIONS.md,
-- Contract, signin-hardening).
--
-- confirm_code is four digits, made at start and returned to the client as TelegramAuthChallenge.confirm_code.
-- The bot shows the same code when it asks the bound account to confirm, so a person who opens a link
-- someone else started sees a code that matches nothing on their own screen. It is not a secret. Rows from
-- before this migration get a random code of their own (the volatile default is evaluated per row while the
-- table is rewritten; the table is small), then the default is dropped, so every new row's code comes from
-- ferry-core.
--
-- token_issued_at is when the status poll handed out the verified challenge's token. The poll sets it with
-- one UPDATE ... WHERE token_issued_at IS NULL, so the token goes out once; later polls answer the status
-- alone. Only a verified row has one.

ALTER TABLE ferry.telegram_challenges
    ADD COLUMN confirm_code text NOT NULL
        DEFAULT lpad(floor(random() * 10000)::int::text, 4, '0')
        CHECK (confirm_code ~ '^[0-9]{4}$'),
    ADD COLUMN token_issued_at timestamptz,
    ADD CONSTRAINT telegram_challenges_token_issued_check
        CHECK (token_issued_at IS NULL OR status = 'verified');

ALTER TABLE ferry.telegram_challenges ALTER COLUMN confirm_code DROP DEFAULT;
