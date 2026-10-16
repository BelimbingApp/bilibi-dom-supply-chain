-- Purchasing 001: each company's suppliers and tax codes, as Bilimbi's
-- `20261012210000_create_supply_chain_purchasing` leaves them (the later
-- Bilimbi migration touched only order lines). Rows of one company carry a
-- `(id, company_id)` unique pair, so the request and order tables of a later
-- version can reference them with composite foreign keys that keep an order's
-- supplier and tax code in its own company.
--
-- Nothing is deleted: triggers refuse DELETE and TRUNCATE, and a supplier or
-- tax code keeps its company and code for good (an order names the code).
-- Deactivation is the only retirement. The two trigger functions are shared
-- by every later Purchasing table.

CREATE TABLE supply_chain_purchasing_suppliers (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  code varchar(50) NOT NULL, name varchar(200) NOT NULL,
  contact_name varchar(200), email varchar(255), phone varchar(50),
  address_line1 varchar(255), address_line2 varchar(255), address_line3 varchar(255),
  locality varchar(120), postcode varchar(20), country_iso varchar(2), admin1_code varchar(20),
  active boolean NOT NULL DEFAULT true,
  inserted_at timestamp(0) NOT NULL, updated_at timestamp(0) NOT NULL,
  CONSTRAINT supply_chain_purchasing_suppliers_id_company_unique UNIQUE (id, company_id),
  CONSTRAINT supply_chain_purchasing_suppliers_company_code_unique UNIQUE (company_id, code),
  CONSTRAINT supply_chain_purchasing_suppliers_country_iso
    CHECK (country_iso IS NULL OR country_iso ~ '^[A-Z]{2}$')
);

CREATE INDEX supply_chain_purchasing_suppliers_tenant_id_company_id_index
  ON supply_chain_purchasing_suppliers (tenant_id, company_id);

CREATE TABLE supply_chain_purchasing_tax_codes (
  id bigserial PRIMARY KEY, tenant_id bigint NOT NULL, company_id bigint NOT NULL,
  code varchar(20) NOT NULL, name varchar(120) NOT NULL, rate numeric(7, 4) NOT NULL,
  active boolean NOT NULL DEFAULT true,
  inserted_at timestamp(0) NOT NULL, updated_at timestamp(0) NOT NULL,
  CONSTRAINT supply_chain_purchasing_tax_codes_id_company_unique UNIQUE (id, company_id),
  CONSTRAINT supply_chain_purchasing_tax_codes_company_code_unique UNIQUE (company_id, code),
  CONSTRAINT supply_chain_purchasing_tax_codes_rate CHECK (rate >= 0 AND rate <= 100)
);

CREATE INDEX supply_chain_purchasing_tax_codes_tenant_id_company_id_index
  ON supply_chain_purchasing_tax_codes (tenant_id, company_id);

CREATE FUNCTION supply_chain_purchasing_refuse_change() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'supply chain purchasing keeps % rows: % is refused',
    TG_TABLE_NAME, TG_OP
    USING ERRCODE = 'restrict_violation';
END
$$;

CREATE FUNCTION supply_chain_purchasing_keep_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF NEW.tenant_id <> OLD.tenant_id OR NEW.company_id <> OLD.company_id
    OR NEW.code <> OLD.code THEN
    RAISE EXCEPTION 'a % row keeps its company and code', TG_TABLE_NAME
      USING ERRCODE = 'restrict_violation';
  END IF;

  RETURN NEW;
END
$$;

CREATE TRIGGER supply_chain_purchasing_suppliers_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_suppliers
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_tax_codes_no_truncate
  BEFORE TRUNCATE ON supply_chain_purchasing_tax_codes
  FOR EACH STATEMENT EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_suppliers_kept
  BEFORE DELETE ON supply_chain_purchasing_suppliers
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_tax_codes_kept
  BEFORE DELETE ON supply_chain_purchasing_tax_codes
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_refuse_change();

CREATE TRIGGER supply_chain_purchasing_suppliers_identity
  BEFORE UPDATE ON supply_chain_purchasing_suppliers
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_keep_identity();

CREATE TRIGGER supply_chain_purchasing_tax_codes_identity
  BEFORE UPDATE ON supply_chain_purchasing_tax_codes
  FOR EACH ROW EXECUTE FUNCTION supply_chain_purchasing_keep_identity();
