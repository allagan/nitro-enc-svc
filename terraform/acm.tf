# ── ACM Certificate for Nitro Enclaves (optional) ────────────────────────────
#
# Enabled when var.tls_domain is non-empty. Provisions a DNS-validated ACM
# certificate and attaches the IAM permissions required by the
# aws-nitro-enclaves-acm agent (p7_proxy) running on the enclave parent node.
#
# FULL INTEGRATION REQUIRES:
#   1. Add CNAME validation record to DNS after `terraform apply`.
#   2. aws-nitro-enclaves-acm installed on enclave nodes (done in userdata when
#      tls_domain is set — see templates/node_userdata.sh.tpl).
#   3. The enclave EIF must be rebuilt to include acm-ray for attestation-bound
#      private key delivery. The acm-ray binary receives the encrypted private
#      key from p7_proxy over vsock, decrypts it using the NSM attestation
#      document, and writes the cert + key to /etc/acm/tls.crt / tls.key.
#      Until the EIF is updated, the enclave falls back to the self-signed cert
#      baked at build time.
#
# Architecture inside the enclave node:
#
#   Parent EC2: p7_proxy (aws-nitro-enclaves-acm)
#               ↑  calls ACM/IAM APIs using the instance role
#               │  writes encrypted private key to vsock port 9005
#               ↓
#   Enclave:   acm-ray → writes /etc/acm/tls.crt + /etc/acm/tls.key
#              (decryption is bound to the enclave attestation document / PCR0)
#
# See: https://docs.aws.amazon.com/enclaves/latest/user/nitro-enclave-refapp.html

locals {
  acm_enabled = var.tls_domain != ""
}

# ── TLS Certificate ───────────────────────────────────────────────────────────

resource "aws_acm_certificate" "enclave_tls" {
  count             = local.acm_enabled ? 1 : 0
  domain_name       = var.tls_domain
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }

  tags = {
    Name        = "nitro-enc-svc-${var.environment}-tls"
    Environment = var.environment
    ManagedBy   = "terraform"
  }
}

# DNS validation records — add these CNAMEs in your DNS provider.
output "acm_certificate_validation_records" {
  description = "DNS CNAME records required to validate the ACM certificate. Add these to your DNS provider."
  value = local.acm_enabled ? {
    for dvo in aws_acm_certificate.enclave_tls[0].domain_validation_options : dvo.domain_name => {
      name  = dvo.resource_record_name
      type  = dvo.resource_record_type
      value = dvo.resource_record_value
    }
  } : {}
}

output "acm_certificate_arn" {
  description = "ARN of the ACM certificate for Nitro Enclaves TLS. Empty when tls_domain is unset."
  value       = local.acm_enabled ? aws_acm_certificate.enclave_tls[0].arn : ""
}

# ── IAM: enclave node permissions for aws-nitro-enclaves-acm ─────────────────

resource "aws_iam_role_policy" "enclave_acm" {
  count = local.acm_enabled ? 1 : 0
  name  = "enclave-acm-${var.environment}"
  role  = aws_iam_role.enclave_node.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      # p7_proxy needs to read the certificate and its private key from ACM.
      {
        Sid    = "AllowACMGetCertificate"
        Effect = "Allow"
        Action = [
          "acm:GetCertificate",
          "acm:DescribeCertificate",
          "acm:ExportCertificate",
        ]
        Resource = aws_acm_certificate.enclave_tls[0].arn
      },
      # p7_proxy enumerates available associations during setup.
      {
        Sid      = "AllowACMListCertificates"
        Effect   = "Allow"
        Action   = ["acm:ListCertificates"]
        Resource = "*"
      },
      # p7_proxy reads the instance role ARN to verify the Nitro association.
      {
        Sid      = "AllowIAMGetRole"
        Effect   = "Allow"
        Action   = ["iam:GetRole"]
        Resource = aws_iam_role.enclave_node.arn
      },
    ]
  })
}
