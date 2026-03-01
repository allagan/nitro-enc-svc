# ── VPC ───────────────────────────────────────────────────────────────────────

output "vpc_id" {
  description = "ID of the VPC."
  value       = aws_vpc.main.id
}

output "private_subnet_ids" {
  description = "IDs of the three private subnets (one per AZ)."
  value       = aws_subnet.private[*].id
}

output "public_subnet_ids" {
  description = "IDs of the three public subnets (one per AZ)."
  value       = aws_subnet.public[*].id
}

# ── EKS ───────────────────────────────────────────────────────────────────────

output "cluster_name" {
  description = "EKS cluster name."
  value       = aws_eks_cluster.main.name
}

output "cluster_endpoint" {
  description = "EKS cluster API server endpoint."
  value       = aws_eks_cluster.main.endpoint
}

output "cluster_certificate_authority_data" {
  description = "Base64-encoded certificate authority data for the EKS cluster."
  value       = aws_eks_cluster.main.certificate_authority[0].data
  sensitive   = true
}

output "cluster_arn" {
  description = "ARN of the EKS cluster."
  value       = aws_eks_cluster.main.arn
}

# ── KMS ───────────────────────────────────────────────────────────────────────

output "kms_dek_key_arn" {
  description = "ARN of the KMS key used to encrypt/decrypt the DEK."
  value       = aws_kms_key.enclave_dek.arn
}

output "kms_dek_key_id" {
  description = "Key ID of the DEK KMS key."
  value       = aws_kms_key.enclave_dek.key_id
}

output "kms_eks_secrets_key_arn" {
  description = "ARN of the KMS key used to encrypt Kubernetes Secrets in etcd."
  value       = aws_kms_key.eks_secrets.arn
}

output "kms_ebs_key_arn" {
  description = "ARN of the KMS key used to encrypt node EBS volumes."
  value       = aws_kms_key.ebs.arn
}

# ── IAM ───────────────────────────────────────────────────────────────────────

output "enclave_node_role_arn" {
  description = "ARN of the IAM role attached to Nitro Enclave nodes."
  value       = aws_iam_role.enclave_node.arn
}

output "codebuild_role_arn" {
  description = "ARN of the IAM role used by CodeBuild."
  value       = aws_iam_role.codebuild.arn
}

# ── ECR ───────────────────────────────────────────────────────────────────────

output "ecr_runner_repository_url" {
  description = "ECR repository URL for the enclave runner image."
  value       = aws_ecr_repository.runner.repository_url
}

output "ecr_vsock_proxy_repository_url" {
  description = "ECR repository URL for the vsock-proxy sidecar image."
  value       = aws_ecr_repository.vsock_proxy.repository_url
}

# ── Storage ───────────────────────────────────────────────────────────────────

output "schemas_bucket_name" {
  description = "Name of the S3 bucket that holds OpenAPI schema files."
  value       = aws_s3_bucket.schemas.id
}

output "pipeline_artifacts_bucket_name" {
  description = "Name of the S3 bucket used for CodePipeline artifacts."
  value       = aws_s3_bucket.pipeline_artifacts.id
}

output "dek_secret_arn" {
  description = "ARN of the Secrets Manager secret that stores the encrypted DEK."
  value       = aws_secretsmanager_secret.dek.arn
}

# ── Pipeline ──────────────────────────────────────────────────────────────────

output "codepipeline_name" {
  description = "Name of the CodePipeline pipeline."
  value       = aws_codepipeline.pipeline.name
}

output "codepipeline_arn" {
  description = "ARN of the CodePipeline pipeline."
  value       = aws_codepipeline.pipeline.arn
}

# ── Quick-start hint ──────────────────────────────────────────────────────────

output "kubeconfig_command" {
  description = "Run this command to configure kubectl after apply."
  value       = "aws eks update-kubeconfig --name ${aws_eks_cluster.main.name} --region ${var.aws_region}"
}
