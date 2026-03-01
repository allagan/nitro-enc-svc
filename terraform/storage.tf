# ── S3: OpenAPI schemas bucket ────────────────────────────────────────────────

resource "aws_s3_bucket" "schemas" {
  bucket = "${var.environment}-nitro-enc-svc-schemas-${var.account_id}"

  tags = {
    Name = "${var.environment}-nitro-enc-svc-schemas"
  }
}

resource "aws_s3_bucket_versioning" "schemas" {
  bucket = aws_s3_bucket.schemas.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "schemas" {
  bucket = aws_s3_bucket.schemas.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "aws:kms"
      kms_master_key_id = aws_kms_key.enclave_dek.arn
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_public_access_block" "schemas" {
  bucket = aws_s3_bucket.schemas.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_policy" "schemas" {
  bucket = aws_s3_bucket.schemas.id

  # Deny all non-HTTPS requests
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "DenyNonHTTPS"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:*"
      Resource = [
        aws_s3_bucket.schemas.arn,
        "${aws_s3_bucket.schemas.arn}/*",
      ]
      Condition = {
        Bool = { "aws:SecureTransport" = "false" }
      }
    }]
  })

  depends_on = [aws_s3_bucket_public_access_block.schemas]
}

# ── S3: CodePipeline artifact bucket ─────────────────────────────────────────

resource "aws_s3_bucket" "pipeline_artifacts" {
  bucket = "${var.environment}-nitro-enc-svc-artifacts-${var.account_id}"

  tags = {
    Name = "${var.environment}-nitro-enc-svc-artifacts"
  }
}

resource "aws_s3_bucket_versioning" "pipeline_artifacts" {
  bucket = aws_s3_bucket.pipeline_artifacts.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "pipeline_artifacts" {
  bucket = aws_s3_bucket.pipeline_artifacts.id

  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "aws:kms"
      kms_master_key_id = aws_kms_key.enclave_dek.arn
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_public_access_block" "pipeline_artifacts" {
  bucket = aws_s3_bucket.pipeline_artifacts.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_policy" "pipeline_artifacts" {
  bucket = aws_s3_bucket.pipeline_artifacts.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "DenyNonHTTPS"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:*"
      Resource = [
        aws_s3_bucket.pipeline_artifacts.arn,
        "${aws_s3_bucket.pipeline_artifacts.arn}/*",
      ]
      Condition = {
        Bool = { "aws:SecureTransport" = "false" }
      }
    }]
  })

  depends_on = [aws_s3_bucket_public_access_block.pipeline_artifacts]
}

# ── Secrets Manager: envelope-encrypted DEK ──────────────────────────────────

resource "aws_secretsmanager_secret" "dek" {
  name        = "nitro-enc-svc/${var.environment}/dek"
  description = "Envelope-encrypted Data Encryption Key (DEK) for nitro-enc-svc. Value is set out-of-band — see docs."
  kms_key_id  = aws_kms_key.enclave_dek.arn

  # 30-day recovery window prevents accidental permanent deletion
  recovery_window_in_days = 30

  tags = {
    Name = "nitro-enc-svc-${var.environment}-dek"
  }
}

# NOTE: No aws_secretsmanager_secret_version resource here.
# The DEK is provisioned out-of-band to keep key material out of Terraform state:
#
#   1. Generate a 32-byte random DEK:
#      openssl rand -hex 32
#
#   2. Wrap it with KMS (produces a ciphertext blob):
#      aws kms encrypt \
#        --key-id <kms_dek_key_id> \
#        --plaintext fileb://<(echo -n "<hex_dek>" | xxd -r -p) \
#        --query CiphertextBlob --output text | base64 -d > dek.bin
#
#   3. Store the ciphertext binary in Secrets Manager:
#      aws secretsmanager put-secret-value \
#        --secret-id nitro-enc-svc/<environment>/dek \
#        --secret-binary fileb://dek.bin
