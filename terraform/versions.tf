terraform {
  required_version = ">= 1.6"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
    helm = {
      source  = "hashicorp/helm"
      version = "~> 2.0"
    }
  }

  # Uncomment and fill in before running `terraform init` in a team/CI environment.
  # backend "s3" {
  #   bucket         = "<your-tfstate-bucket>"
  #   key            = "nitro-enc-svc/<environment>/terraform.tfstate"
  #   region         = "<your-region>"
  #   dynamodb_table = "<your-lock-table>"
  #   encrypt        = true
  # }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Project     = "nitro-enc-svc"
      Environment = var.environment
      ManagedBy   = "terraform"
    }
  }
}

provider "helm" {
  kubernetes {
    host                   = aws_eks_cluster.main.endpoint
    cluster_ca_certificate = base64decode(aws_eks_cluster.main.certificate_authority[0].data)
    exec {
      api_version = "client.authentication.k8s.io/v1beta1"
      command     = "aws"
      args        = ["eks", "get-token", "--cluster-name", aws_eks_cluster.main.name, "--region", var.aws_region]
    }
  }
}
