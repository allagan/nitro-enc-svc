# ── Control plane log group ───────────────────────────────────────────────────

resource "aws_cloudwatch_log_group" "eks_cluster" {
  name              = "/aws/eks/${var.cluster_name}/cluster"
  retention_in_days = 90

  tags = {
    Name = "${var.cluster_name}-control-plane-logs"
  }
}

# ── EKS Cluster ───────────────────────────────────────────────────────────────

resource "aws_eks_cluster" "main" {
  name     = var.cluster_name
  version  = var.cluster_version
  role_arn = aws_iam_role.eks_cluster.arn

  enabled_cluster_log_types = [
    "api",
    "audit",
    "authenticator",
    "controllerManager",
    "scheduler",
  ]

  vpc_config {
    subnet_ids              = aws_subnet.private[*].id
    endpoint_private_access = true
    endpoint_public_access  = true
    public_access_cidrs     = var.public_access_cidrs
  }

  encryption_config {
    provider {
      key_arn = aws_kms_key.eks_secrets.arn
    }
    resources = ["secrets"]
  }

  access_config {
    authentication_mode = "API_AND_CONFIG_MAP"
  }

  depends_on = [
    aws_iam_role_policy_attachment.eks_cluster_policy,
    aws_cloudwatch_log_group.eks_cluster,
  ]

  tags = {
    Name = var.cluster_name
  }
}

# ── General node group (system workloads) ─────────────────────────────────────

resource "aws_eks_node_group" "general" {
  cluster_name    = aws_eks_cluster.main.name
  node_group_name = "${var.cluster_name}-general"
  node_role_arn   = aws_iam_role.general_node.arn
  subnet_ids      = aws_subnet.private[*].id

  instance_types = var.general_instance_types

  scaling_config {
    desired_size = var.general_desired_size
    min_size     = var.general_min_size
    max_size     = var.general_max_size
  }

  update_config {
    max_unavailable = 1
  }

  taint {
    key    = "CriticalAddonsOnly"
    value  = "true"
    effect = "NO_SCHEDULE"
  }

  depends_on = [
    aws_iam_role_policy_attachment.general_node_worker,
    aws_iam_role_policy_attachment.general_node_cni,
    aws_iam_role_policy_attachment.general_node_ecr,
  ]

  tags = {
    Name = "${var.cluster_name}-general"
  }
}

# ── Nitro Enclave launch template ─────────────────────────────────────────────

data "aws_ssm_parameter" "eks_al2_ami" {
  name = "/aws/service/eks/optimized-ami/${var.cluster_version}/amazon-linux-2/recommended/image_id"
}

resource "aws_launch_template" "nitro_enclave" {
  name_prefix = "${var.cluster_name}-nitro-"
  description = "Launch template for EKS nodes with Nitro Enclave support"

  image_id      = data.aws_ssm_parameter.eks_al2_ami.value
  instance_type = var.nitro_instance_types[0]

  metadata_options {
    http_tokens                 = "required"
    http_put_response_hop_limit = 2
    http_endpoint               = "enabled"
  }

  enclave_options {
    enabled = true
  }

  block_device_mappings {
    device_name = "/dev/xvda"
    ebs {
      volume_type           = "gp3"
      volume_size           = 50
      encrypted             = true
      kms_key_id            = aws_kms_key.ebs.arn
      delete_on_termination = true
    }
  }

  user_data = base64encode(templatefile("${path.module}/templates/node_userdata.sh.tpl", {
    cluster_name      = var.cluster_name
    enclave_memory_mb = var.enclave_memory_mb
    enclave_cpu_count = var.enclave_cpu_count
    aws_region        = var.aws_region
    # Pass the ACM cert ARN when tls_domain is configured; empty string otherwise.
    # The userdata skips aws-nitro-enclaves-acm installation when this is empty.
    acm_cert_arn = local.acm_enabled ? aws_acm_certificate.enclave_tls[0].arn : ""
  }))

  tag_specifications {
    resource_type = "instance"
    tags = {
      Name        = "${var.cluster_name}-nitro-node"
      Environment = var.environment
      ManagedBy   = "terraform"
    }
  }

  tag_specifications {
    resource_type = "volume"
    tags = {
      Name        = "${var.cluster_name}-nitro-node-volume"
      Environment = var.environment
      ManagedBy   = "terraform"
    }
  }

  lifecycle {
    create_before_destroy = true
  }
}

# ── Nitro Enclave node group ──────────────────────────────────────────────────

resource "aws_eks_node_group" "nitro" {
  cluster_name    = aws_eks_cluster.main.name
  node_group_name = "${var.cluster_name}-nitro"
  node_role_arn   = aws_iam_role.enclave_node.arn
  subnet_ids      = [aws_subnet.private[0].id] # single AZ keeps vsock-proxy co-located

  launch_template {
    id      = aws_launch_template.nitro_enclave.id
    version = "$Latest"
  }

  scaling_config {
    desired_size = var.nitro_desired_size
    min_size     = var.nitro_min_size
    max_size     = var.nitro_max_size
  }

  update_config {
    max_unavailable = 1
  }

  labels = {
    "aws.amazon.com/nitro-enclaves" = "true"
  }

  depends_on = [
    aws_iam_role_policy_attachment.enclave_node_worker,
    aws_iam_role_policy_attachment.enclave_node_ecr,
    aws_iam_role_policy.enclave_kms,
    aws_iam_role_policy.enclave_secretsmanager,
    aws_iam_role_policy.enclave_s3,
    aws_eks_access_entry.enclave_node,
  ]

  tags = {
    Name = "${var.cluster_name}-nitro"
  }
}

# ── Karpenter (Helm) ──────────────────────────────────────────────────────────

resource "helm_release" "karpenter" {
  namespace        = "kube-system"
  name             = "karpenter"
  repository       = "oci://public.ecr.aws/karpenter"
  chart            = "karpenter"
  version          = var.karpenter_version
  create_namespace = false

  values = [yamlencode({
    settings = {
      clusterName     = aws_eks_cluster.main.name
      clusterEndpoint = aws_eks_cluster.main.endpoint
    }
    tolerations = [{
      key      = "CriticalAddonsOnly"
      operator = "Exists"
    }]
    affinity = {
      nodeAffinity = {
        requiredDuringSchedulingIgnoredDuringExecution = {
          nodeSelectorTerms = [{
            matchExpressions = [{
              key      = "kubernetes.io/os"
              operator = "In"
              values   = ["linux"]
            }]
          }]
        }
      }
    }
  })]

  depends_on = [
    aws_eks_node_group.general,
    aws_eks_addon.pod_identity_agent,
    aws_eks_pod_identity_association.karpenter,
  ]
}

# ── EKS access entry — enclave node role (allows Karpenter-launched nodes to join) ──

resource "aws_eks_access_entry" "enclave_node" {
  cluster_name  = aws_eks_cluster.main.name
  principal_arn = aws_iam_role.enclave_node.arn
  type          = "EC2_LINUX"

  depends_on = [aws_eks_cluster.main]
}

# ── EKS access entry — test CodeBuild role (kubectl view access) ──────────────

resource "aws_eks_access_entry" "codebuild_test" {
  cluster_name  = aws_eks_cluster.main.name
  principal_arn = aws_iam_role.codebuild_test.arn
  type          = "STANDARD"
}

resource "aws_eks_access_policy_association" "codebuild_test" {
  cluster_name  = aws_eks_cluster.main.name
  principal_arn = aws_iam_role.codebuild_test.arn
  policy_arn    = "arn:aws:eks::aws:cluster-access-policy/AmazonEKSEditPolicy"

  access_scope {
    type = "cluster"
  }

  depends_on = [aws_eks_access_entry.codebuild_test]
}

# ── EKS Pod Identity Agent (must be installed before addon Pod Identity associations) ──

resource "aws_eks_addon" "pod_identity_agent" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "eks-pod-identity-agent"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_node_group.general,
  ]

  tags = {
    Name = "${var.cluster_name}-pod-identity-agent"
  }
}

# ── Core addons ───────────────────────────────────────────────────────────────

resource "aws_eks_addon" "vpc_cni" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "vpc-cni"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_pod_identity_association.vpc_cni,
  ]

  tags = {
    Name = "${var.cluster_name}-vpc-cni"
  }
}

resource "aws_eks_addon" "coredns" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "coredns"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_node_group.general,
  ]

  tags = {
    Name = "${var.cluster_name}-coredns"
  }
}

resource "aws_eks_addon" "kube_proxy" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "kube-proxy"
  resolve_conflicts_on_update = "OVERWRITE"

  tags = {
    Name = "${var.cluster_name}-kube-proxy"
  }
}

resource "aws_eks_addon" "ebs_csi_driver" {
  cluster_name                = aws_eks_cluster.main.name
  addon_name                  = "aws-ebs-csi-driver"
  resolve_conflicts_on_update = "OVERWRITE"

  depends_on = [
    aws_eks_pod_identity_association.ebs_csi,
    aws_eks_node_group.general,
  ]

  tags = {
    Name = "${var.cluster_name}-ebs-csi-driver"
  }
}

# ── Cluster admin access entries ──────────────────────────────────────────

resource "aws_eks_access_entry" "admin" {
  for_each = toset(var.cluster_admin_arns)

  cluster_name  = aws_eks_cluster.main.name
  principal_arn = each.key
  type          = "STANDARD"
}

resource "aws_eks_access_policy_association" "admin" {
  for_each = toset(var.cluster_admin_arns)

  cluster_name  = aws_eks_cluster.main.name
  principal_arn = each.key
  policy_arn    = "arn:aws:eks::aws:cluster-access-policy/AmazonEKSClusterAdminPolicy"

  access_scope {
    type = "cluster"
  }

  depends_on = [aws_eks_access_entry.admin]
}

# ── Pod Identity associations ─────────────────────────────────────────────────

resource "aws_eks_pod_identity_association" "vpc_cni" {
  cluster_name    = aws_eks_cluster.main.name
  namespace       = "kube-system"
  service_account = "aws-node"
  role_arn        = aws_iam_role.vpc_cni_pod_identity.arn

  depends_on = [aws_eks_addon.pod_identity_agent]
}

resource "aws_eks_pod_identity_association" "ebs_csi" {
  cluster_name    = aws_eks_cluster.main.name
  namespace       = "kube-system"
  service_account = "ebs-csi-controller-sa"
  role_arn        = aws_iam_role.ebs_csi_pod_identity.arn

  depends_on = [aws_eks_addon.pod_identity_agent]
}
