CREATE SCHEMA inventory;
CREATE TYPE public.stock_state AS ENUM ('available', 'reserved', 'sold');

CREATE TABLE inventory.products (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  sku STRING NOT NULL UNIQUE,
  state public.stock_state NOT NULL DEFAULT 'available',
  quantity INT NOT NULL DEFAULT 0,
  CONSTRAINT products_quantity_check CHECK (quantity >= 0)
);

CREATE INDEX products_state_idx ON inventory.products (state) WHERE quantity > 0;

CREATE VIEW public.available_products AS
  SELECT id, sku, quantity
  FROM inventory.products
  WHERE state = 'available' AND quantity > 0;
