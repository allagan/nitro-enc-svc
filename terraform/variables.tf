variable "aws_region" {
  description = "AWS region to deploy into (e.g. us-east-1)."
  type        = string
}

variable "environment" {
  description = "Deployment environment: dev, staging, or prod."
  type        = string
  validation {
    condition     = contains(["dev", "staging", "prod"], var.environment)
    error_message = "environment must be one of: dev, staging, prod."
  }
}

variable "account_id" {
  description = "12-digit AWS account ID."
  type        = string
  validation {
    condition     = can(regex("^[0-9]{12}$", var.account_id))
    error_message = "account_id must be a 12-digit number."
  }
}

# ── EKS ───────────────────────────────────────────────────────────────────────

variable "cluster_name" {
  description = "Name of the EKS cluster."
  type        = string
}

variable "cluster_version" {
  description = "Kubernetes version for the EKS cluster."
  type        = string
  default     = "1.31"
}

variable "public_access_cidrs" {
  description = "CIDR blocks allowed to reach the EKS public API endpoint."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "cluster_admin_arns" {
  description = "List of IAM principal ARNs (users/roles) granted cluster-admin access via EKS access entries."
  type        = list(string)
  default     = []
}

# ── VPC ───────────────────────────────────────────────────────────────────────

variable "vpc_cidr" {
  description = "CIDR block for the VPC."
  type        = string
  default     = "10.0.0.0/16"
}

variable "single_nat_gateway" {
  description = "Use a single NAT gateway instead of one per AZ. Reduces cost for dev/test environments."
  type        = bool
  default     = false
}

variable "availability_zones" {
  description = "List of exactly 3 availability zones."
  type        = list(string)
  validation {
    condition     = length(var.availability_zones) == 3
    error_message = "Exactly 3 availability zones are required."
  }
}

# ── Node groups ───────────────────────────────────────────────────────────────

variable "general_instance_types" {
  description = "EC2 instance types for the general-purpose node group."
  type        = list(string)
  default     = ["m5.large"]
}

variable "general_desired_size" {
  description = "Desired number of general-purpose nodes."
  type        = number
  default     = 2
}

variable "general_min_size" {
  description = "Minimum number of general-purpose nodes."
  type        = number
  default     = 1
}

variable "general_max_size" {
  description = "Maximum number of general-purpose nodes."
  type        = number
  default     = 4
}

variable "nitro_instance_types" {
  description = "EC2 instance types for the Nitro Enclave-capable node group (must support enclaves)."
  type        = list(string)
  default     = ["c5.xlarge"]
}

variable "nitro_desired_size" {
  description = "Desired number of Nitro Enclave nodes."
  type        = number
  default     = 2
}

variable "nitro_min_size" {
  description = "Minimum number of Nitro Enclave nodes."
  type        = number
  default     = 1
}

variable "nitro_max_size" {
  description = "Maximum number of Nitro Enclave nodes."
  type        = number
  default     = 4
}

# ── Enclave runtime ───────────────────────────────────────────────────────────

variable "enclave_memory_mb" {
  description = "MiB of host memory to reserve for the Nitro Enclave."
  type        = number
  default     = 2048
}

variable "enclave_cpu_count" {
  description = "Number of vCPUs to reserve for the Nitro Enclave."
  type        = number
  default     = 2
}

variable "enclave_cid" {
  description = "Vsock CID assigned to the Nitro Enclave."
  type        = number
  default     = 16
}

variable "vsock_proxy_cid" {
  description = "Vsock CID of the parent EC2 aws-vsock-proxy that forwards AWS API calls."
  type        = string
}

# ── KMS / attestation ─────────────────────────────────────────────────────────

variable "kms_enclave_pcr0" {
  description = <<-EOT
    SHA-384 PCR0 hash of the enclave image file (EIF), used to gate KMS Decrypt.
    Leave empty ("") to provision without the attestation constraint — the enclave
    cannot decrypt the DEK until this is set and re-applied after the first build.
  EOT
  type        = string
  default     = ""
}

# ── Observability ─────────────────────────────────────────────────────────────

variable "otel_otlp_endpoint" {
  description = "OTLP/gRPC endpoint (vsock address) the enclave exports telemetry to."
  type        = string
}

# ── Source / CI-CD ────────────────────────────────────────────────────────────

variable "codestar_connection_arn" {
  description = "ARN of the AWS CodeStar connection to GitHub (created once in the console)."
  type        = string
}

variable "source_repo_id" {
  description = "GitHub repository in 'org/repo' format."
  type        = string
}

variable "source_repo_branch" {
  description = "Branch that triggers the CodePipeline."
  type        = string
  default     = "main"
}

# ── Service configuration ─────────────────────────────────────────────────────

variable "log_level" {
  description = "Log level for the enclave service (trace, debug, info, warn, error)."
  type        = string
  default     = "info"
}

variable "s3_prefix" {
  description = "S3 key prefix under which OpenAPI schema files are stored."
  type        = string
  default     = "schemas/"
}

variable "schema_header_name" {
  description = "HTTP request header the enclave uses to select the OpenAPI schema."
  type        = string
  default     = "X-Schema-Name"
}

variable "dek_rotation_interval_secs" {
  description = "Seconds between background DEK rotation checks."
  type        = number
  default     = 3600
}

variable "schema_refresh_interval_secs" {
  description = "Seconds between background schema cache refresh runs."
  type        = number
  default     = 300
}

variable "vsock_proxy_port" {
  description = "Vsock port on the parent EC2 that the aws-vsock-proxy listens on."
  type        = number
  default     = 8000
}

variable "tls_port" {
  description = "TCP port the enclave HTTPS server listens on inside the enclave."
  type        = number
  default     = 443
}
