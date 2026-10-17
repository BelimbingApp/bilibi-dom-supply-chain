-- Mirrors Bilimbi.SupplyChain.Receiving.Migrations.CreateReceiving,
-- receiving/priv/repo/migrations/20261013110000_create_supply_chain_receiving.exs:
-- permanent fresh-schema statements, including Receiving-owned functions and triggers.
CREATE TABLE supply_chain_receiving_settings (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  over_receipt_tolerance_percent numeric(7, 4) NOT NULL,
  set_by_user_id bigint NOT NULL, set_by_impersonator_id bigint,
  occurred_at timestamp(6) NOT NULL,
  CONSTRAINT supply_chain_receiving_settings_tolerance
    CHECK (over_receipt_tolerance_percent >= 0 AND over_receipt_tolerance_percent <= 100)
);
CREATE INDEX supply_chain_receiving_settings_company_id_id_index
  ON supply_chain_receiving_settings (company_id, id);

CREATE TABLE supply_chain_receiving_receipts (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  receipt_number bigint NOT NULL, kind varchar(20) NOT NULL DEFAULT 'receipt',
  status varchar(20) NOT NULL DEFAULT 'draft',
  order_id bigint NOT NULL, order_number bigint NOT NULL, supplier_id bigint NOT NULL,
  reverses_receipt_id bigint, reason varchar(500),
  received_on date NOT NULL, supplier_reference varchar(100), notes varchar(1000),
  created_by_user_id bigint NOT NULL, created_by_impersonator_id bigint,
  posted_by_user_id bigint, posted_by_impersonator_id bigint, posted_at timestamp(6),
  cancelled_at timestamp(6),
  inserted_at timestamp(0) NOT NULL, updated_at timestamp(0) NOT NULL,
  CONSTRAINT supply_chain_receiving_receipts_id_company_unique UNIQUE (id, company_id),
  CONSTRAINT supply_chain_receiving_receipts_company_number_unique UNIQUE (company_id, receipt_number),
  CONSTRAINT supply_chain_receiving_receipts_reverses_fkey
    FOREIGN KEY (reverses_receipt_id, company_id)
    REFERENCES supply_chain_receiving_receipts (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_receiving_receipts_receipt_number CHECK (receipt_number > 0),
  CONSTRAINT supply_chain_receiving_receipts_order_number CHECK (order_number > 0),
  CONSTRAINT supply_chain_receiving_receipts_kind CHECK (kind IN ('receipt', 'reversal')),
  CONSTRAINT supply_chain_receiving_receipts_status CHECK (status IN ('draft', 'posted', 'cancelled')),
  CONSTRAINT supply_chain_receiving_receipts_reversal CHECK (
    (kind = 'receipt' AND reverses_receipt_id IS NULL AND reason IS NULL)
    OR (kind = 'reversal' AND reverses_receipt_id IS NOT NULL AND reason IS NOT NULL)
  ),
  CONSTRAINT supply_chain_receiving_receipts_not_self_reversal
    CHECK (reverses_receipt_id IS NULL OR reverses_receipt_id <> id),
  CONSTRAINT supply_chain_receiving_receipts_lifecycle CHECK (
    (status = 'draft' AND posted_by_user_id IS NULL AND posted_by_impersonator_id IS NULL
      AND posted_at IS NULL AND cancelled_at IS NULL)
    OR (status = 'posted' AND posted_by_user_id IS NOT NULL AND posted_at IS NOT NULL
      AND cancelled_at IS NULL)
    OR (status = 'cancelled' AND posted_by_user_id IS NULL AND posted_by_impersonator_id IS NULL
      AND posted_at IS NULL AND cancelled_at IS NOT NULL)
  )
);
CREATE UNIQUE INDEX supply_chain_receiving_receipts_reversal_unique
  ON supply_chain_receiving_receipts (reverses_receipt_id) WHERE status <> 'cancelled';
CREATE INDEX supply_chain_receiving_receipts_tenant_company_status_index
  ON supply_chain_receiving_receipts (tenant_id, company_id, status);
CREATE INDEX supply_chain_receiving_receipts_company_id_order_id_index
  ON supply_chain_receiving_receipts (company_id, order_id);

CREATE TABLE supply_chain_receiving_receipt_lines (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  receipt_id bigint NOT NULL, line_number integer NOT NULL,
  order_line_id bigint NOT NULL, order_line_number integer NOT NULL,
  description varchar(500) NOT NULL, unit varchar(20) NOT NULL,
  ordered_quantity numeric(20, 4) NOT NULL,
  received_quantity numeric(20, 4) NOT NULL DEFAULT 0,
  rejected_quantity numeric(20, 4) NOT NULL DEFAULT 0,
  rejection_reason varchar(500),
  CONSTRAINT supply_chain_receiving_receipt_lines_receipt_fkey
    FOREIGN KEY (receipt_id, company_id)
    REFERENCES supply_chain_receiving_receipts (id, company_id) ON DELETE RESTRICT,
  CONSTRAINT supply_chain_receiving_receipt_lines_receipt_line_unique UNIQUE (receipt_id, line_number),
  CONSTRAINT supply_chain_receiving_receipt_lines_receipt_order_line_unique UNIQUE (receipt_id, order_line_id),
  CONSTRAINT supply_chain_receiving_receipt_lines_line_number CHECK (line_number > 0),
  CONSTRAINT supply_chain_receiving_receipt_lines_order_line_number CHECK (order_line_number > 0),
  CONSTRAINT supply_chain_receiving_receipt_lines_ordered_quantity CHECK (ordered_quantity > 0),
  CONSTRAINT supply_chain_receiving_receipt_lines_quantities CHECK (
    received_quantity >= 0 AND rejected_quantity >= 0
    AND received_quantity + rejected_quantity > 0
  ),
  CONSTRAINT supply_chain_receiving_receipt_lines_rejection CHECK (
    (rejected_quantity = 0 AND rejection_reason IS NULL)
    OR (rejected_quantity > 0 AND rejection_reason IS NOT NULL)
  )
);
CREATE INDEX supply_chain_receiving_receipt_lines_company_order_line_index
  ON supply_chain_receiving_receipt_lines (company_id, order_line_id);

CREATE TABLE supply_chain_receiving_receipt_events (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  receipt_id bigint NOT NULL, from_status varchar(20), to_status varchar(20) NOT NULL,
  actor_user_id bigint NOT NULL, actor_impersonator_id bigint, reason varchar(500),
  occurred_at timestamp(6) NOT NULL,
  CONSTRAINT supply_chain_receiving_receipt_events_receipt_fkey
    FOREIGN KEY (receipt_id, company_id)
    REFERENCES supply_chain_receiving_receipts (id, company_id) ON DELETE RESTRICT
);
CREATE INDEX supply_chain_receiving_receipt_events_receipt_id_id_index
  ON supply_chain_receiving_receipt_events (receipt_id, id);
CREATE INDEX supply_chain_receiving_receipt_events_company_status_id_index
  ON supply_chain_receiving_receipt_events (company_id, to_status, id);

CREATE FUNCTION supply_chain_receiving_refuse_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'supply chain receiving keeps % rows: % is refused', TG_TABLE_NAME, TG_OP
    USING ERRCODE = 'restrict_violation';
END
$$;

CREATE FUNCTION supply_chain_receiving_stamp() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  NEW.occurred_at := clock_timestamp() AT TIME ZONE 'UTC';
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_receiving_number_receipt() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.status <> 'draft' THEN
    RAISE EXCEPTION 'a new goods receipt is a draft' USING ERRCODE = 'restrict_violation';
  END IF;
  NEW.receipt_number := COALESCE(
    (SELECT max(receipt_number) FROM supply_chain_receiving_receipts
     WHERE company_id = NEW.company_id), 0) + 1;
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_receiving_receipt_header_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.receipt_number <> OLD.receipt_number OR NEW.kind <> OLD.kind
    OR NEW.order_id <> OLD.order_id OR NEW.order_number <> OLD.order_number
    OR NEW.supplier_id <> OLD.supplier_id
    OR NEW.reverses_receipt_id IS DISTINCT FROM OLD.reverses_receipt_id
    OR NEW.reason IS DISTINCT FROM OLD.reason
    OR NEW.created_by_user_id <> OLD.created_by_user_id
    OR NEW.created_by_impersonator_id IS DISTINCT FROM OLD.created_by_impersonator_id THEN
    RAISE EXCEPTION 'a goods receipt keeps its company, number, kind, order and creator'
      USING ERRCODE = 'restrict_violation';
  END IF;
  IF NEW.status <> OLD.status THEN
    IF (OLD.status, NEW.status) NOT IN (('draft', 'posted'), ('draft', 'cancelled')) THEN
      RAISE EXCEPTION 'a goods receipt does not go from % to %', OLD.status, NEW.status
        USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.received_on <> OLD.received_on
      OR NEW.supplier_reference IS DISTINCT FROM OLD.supplier_reference
      OR NEW.notes IS DISTINCT FROM OLD.notes THEN
      RAISE EXCEPTION 'a goods receipt is not edited in the same change as a transition'
        USING ERRCODE = 'restrict_violation';
    END IF;
  ELSIF OLD.status <> 'draft' THEN
    RAISE EXCEPTION 'goods receipt % is %: it is fixed', OLD.id, OLD.status
      USING ERRCODE = 'restrict_violation';
  END IF;
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_receiving_receipt_line_rules() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  receipt_status varchar(20);
  subject_id bigint;
BEGIN
  IF TG_OP = 'DELETE' THEN
    subject_id := OLD.receipt_id;
  ELSE
    subject_id := NEW.receipt_id;
  END IF;
  IF TG_OP = 'UPDATE' AND (NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.receipt_id <> OLD.receipt_id) THEN
    RAISE EXCEPTION 'a goods receipt line keeps its receipt' USING ERRCODE = 'restrict_violation';
  END IF;
  SELECT status INTO receipt_status FROM supply_chain_receiving_receipts WHERE id = subject_id;
  IF receipt_status IS DISTINCT FROM 'draft' THEN
    RAISE EXCEPTION 'goods receipt % is %: its lines are fixed', subject_id, receipt_status
      USING ERRCODE = 'restrict_violation';
  END IF;
  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;
  RETURN NEW;
END
$$;

CREATE FUNCTION supply_chain_receiving_receipt_audited() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  previous varchar(20);
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF NEW.status = OLD.status THEN
      RETURN NULL;
    END IF;
    previous := OLD.status;
  END IF;
  IF NOT EXISTS (
    SELECT 1 FROM supply_chain_receiving_receipt_events
    WHERE receipt_id = NEW.id AND from_status IS NOT DISTINCT FROM previous
      AND to_status = NEW.status AND occurred_at >= now() AT TIME ZONE 'UTC'
  ) THEN
    RAISE EXCEPTION 'goods receipt % became % without a history row', NEW.id, NEW.status
      USING ERRCODE = 'check_violation';
  END IF;
  RETURN NULL;
END
$$;

CREATE TRIGGER supply_chain_receiving_settings_no_truncate
  BEFORE TRUNCATE ON supply_chain_receiving_settings
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_receipts_no_truncate
  BEFORE TRUNCATE ON supply_chain_receiving_receipts
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_receipt_lines_no_truncate
  BEFORE TRUNCATE ON supply_chain_receiving_receipt_lines
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_receipt_events_no_truncate
  BEFORE TRUNCATE ON supply_chain_receiving_receipt_events
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_receipts_kept
  BEFORE DELETE ON supply_chain_receiving_receipts
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_settings_append_only
  BEFORE UPDATE OR DELETE ON supply_chain_receiving_settings
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_receipt_events_append_only
  BEFORE UPDATE OR DELETE ON supply_chain_receiving_receipt_events
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_refuse_change();
CREATE TRIGGER supply_chain_receiving_settings_stamp
  BEFORE INSERT ON supply_chain_receiving_settings
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_stamp();
CREATE TRIGGER supply_chain_receiving_receipt_events_stamp
  BEFORE INSERT ON supply_chain_receiving_receipt_events
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_stamp();
CREATE TRIGGER supply_chain_receiving_receipts_number
  BEFORE INSERT ON supply_chain_receiving_receipts
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_number_receipt();
CREATE TRIGGER supply_chain_receiving_receipts_header
  BEFORE UPDATE ON supply_chain_receiving_receipts
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_receipt_header_rules();
CREATE TRIGGER supply_chain_receiving_receipt_lines_rules
  BEFORE INSERT OR UPDATE OR DELETE ON supply_chain_receiving_receipt_lines
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_receipt_line_rules();
CREATE CONSTRAINT TRIGGER supply_chain_receiving_receipts_audited
  AFTER INSERT OR UPDATE ON supply_chain_receiving_receipts
  DEFERRABLE INITIALLY DEFERRED
  FOR EACH ROW EXECUTE FUNCTION supply_chain_receiving_receipt_audited();
